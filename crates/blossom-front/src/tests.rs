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

// ---------------------------------------------------------------- pure functions (LANGUAGE §16.1)

#[test]
fn functions_with_lets_closures_and_the_library_compile() {
    let src = with_head(
        "struct Cur { buf: Bytes, pos: u64 }\n\
         fn take(c: Cur, k: u64) -> Option<(Bytes, Cur)> {\n\
             c.buf.slice(c.pos, c.pos + k).map(|x| (x, Cur { buf: c.buf, pos: c.pos + k }))\n\
         }\n\
         fn total(n: u64) -> u64 {\n\
             let xs = range(0, n);\n\
             let ys = xs.map(|x| x * 2).filter(|y| y > 3);\n\
             ys.fold(0, |acc, y| acc + y)\n\
         }\n\
         fn first_two(b: Bytes) -> Option<Bytes> {\n\
             take(Cur { buf: b, pos: 0 }, 2).map(|p| p.0)\n\
         }\n\
         output out(k: u64, t: u64);\n\
         a: on go(k, v) { emit out(k, total(v)); }\n",
    );
    assert_eq!(codes(src), Vec::<String>::new());
}

#[test]
fn a_recursive_function_is_bls0213() {
    let src = with_head(
        "fn f(n: u64) -> u64 { if n == 0 { 0 } else { f(n - 1) } }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(f(k)); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0213"]);
}

#[test]
fn mutually_recursive_functions_are_bls0213_each() {
    let src = with_head(
        "fn f(n: u64) -> u64 { g(n) }\n\
         fn g(n: u64) -> u64 { f(n) }\n\
         fn h(n: u64) -> u64 { n }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(h(k)); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0213", "BLS0213"]);
}

#[test]
fn a_function_calling_one_declared_after_it_compiles() {
    let src = with_head(
        "fn f(n: u64) -> u64 { g(n) + 1 }\n\
         fn g(n: u64) -> u64 { n * 2 }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(f(k)); }\n",
    );
    assert_eq!(codes(src), Vec::<String>::new());
}

#[test]
fn a_let_block_outside_a_function_is_bls0214() {
    let src = with_head(
        "output out(k: u64);\n\
         view w(x) = go(k, _), let x = if k > 0 { let y = k; y } else { k };\n",
    );
    assert_eq!(codes(src), vec!["BLS0214"]);
}

#[test]
fn a_closure_outside_a_function_is_bls0214() {
    let src = with_head(
        "output out(k: u64);\n\
         a: on go(k, v), let w = [k, v].map(|x| x + 1) { emit out(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0214"]);
}

#[test]
fn a_function_reading_a_relation_or_the_clock_is_bls0215() {
    for body in ["now()", "tick()", "self", "c", "rand_range(0, 3, n)"] {
        let src = with_head(Box::leak(
            format!(
                "cell c: LMax<u64>;\n\
                 fn f(n: u64) -> u64 {{ let x = {body}; n }}\n\
                 output out(k: u64);\n\
                 a: on go(k, v) {{ emit out(f(k)); }}\n"
            )
            .into_boxed_str(),
        ));
        assert_eq!(codes(src), vec!["BLS0215"], "{body}");
    }
}

#[test]
fn function_classes_and_lattice_parameters_are_not_implemented() {
    let class = with_head(
        "monotone fn f(n: u64) -> u64 { n }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(f(k)); }\n",
    );
    assert!(codes(class).contains(&"BLS0908".to_owned()), "{:?}", codes(class));
    let lattice = with_head(
        "fn f(n: LMax<u64>) -> bool { n >= 3 }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert!(codes(lattice).contains(&"BLS0908".to_owned()), "{:?}", codes(lattice));
}

#[test]
fn a_call_with_the_wrong_arity_is_bls0301() {
    let src = with_head(
        "fn f(a: u64, b: u64) -> u64 { a + b }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(f(k)); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0301"]);
}

#[test]
fn a_body_of_the_wrong_type_is_bls0300() {
    let src = with_head(
        "fn f(a: u64) -> String { a }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0300"]);
}

#[test]
fn a_combinator_needs_a_closure_and_a_plain_method_takes_none() {
    let no_closure = with_head(
        "fn f(n: u64) -> Vec<u64> { range(0, n).map(n) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(no_closure), vec!["BLS0300"]);
    let stray = with_head(
        "fn f(n: u64) -> Option<u64> { range(0, n).get(|x| x) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(stray), vec!["BLS0300"]);
    let arity = with_head(
        "fn f(n: u64) -> u64 { range(0, n).fold(0, |acc| acc) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(arity), vec!["BLS0301"]);
}

#[test]
fn a_function_named_like_a_relation_is_bls0201() {
    let src = with_head(
        "fn go(n: u64) -> u64 { n }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0201"]);
}

#[test]
fn a_refutable_let_pattern_is_bls0301() {
    let src = with_head(
        "fn f(n: Option<u64>) -> u64 { let Some(x) = n; x }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0301"]);
}

#[test]
fn an_else_if_chain_keeps_its_last_branch() {
    // The S6 functions work found `else if` dropped its `else` (an internal error, "an `if` value without `else`").
    let src = with_head(
        "fn sign(n: i64) -> i64 { if n > 0 { 1 } else if n < 0 { 0 - 1 } else { 0 } }\n\
         output out(k: u64);\n\
         view w(x) = go(k, _), let x = if k > 5 { 2 } else if k > 2 { 1 } else { 0 };\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(src), Vec::<String>::new());
}

#[test]
fn a_function_passed_as_a_value_is_not_implemented() {
    let src = with_head(
        "fn keep(x: u64) -> bool { x > 1 }\n\
         cell s: LSet<u64>;\n\
         output out(k: u64);\n\
         a: while let t = s.filter(keep) { emit out(0); }\n",
    );
    assert!(codes(src).contains(&"BLS0908".to_owned()), "{:?}", codes(src));
}

#[test]
fn byte_primitives_type_check_and_reject_unsupported_widths() {
    let ok = with_head(
        "fn hdr(b: Bytes) -> Option<(i16, i32, u64)> {\n\
             b.i16_be_at(0).and_then(|k| b.i32_be_at(2).and_then(|c| b.uvarint_at(6).map(|v| (k, c, v.0))))\n\
         }\n\
         fn frame(body: Bytes) -> Bytes { Bytes::join([Bytes::from_i32_be(4), body, Bytes::varint(0 - 1)]) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(ok), Vec::<String>::new());
    let wide = with_head(
        "fn f(b: Bytes) -> Option<u128> { b.u128_be_at(0) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert!(codes(wide).contains(&"BLS0908".to_owned()), "{:?}", codes(wide));
    let ctor = with_head(
        "fn f(x: u64) -> Bytes { Bytes::from_u64_le(x) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert!(codes(ctor).contains(&"BLS0908".to_owned()), "{:?}", codes(ctor));
    let arity = with_head(
        "fn f(b: Bytes) -> Option<Bytes> { b.put_u16_be(0) }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(arity), vec!["BLS0301"]);
}

#[test]
fn extern_fns_are_checked_against_the_standard_catalog() {
    let ok = with_head(
        "extern fn crc32c(b: Bytes) -> u32 = \"blossom_std::checksum::crc32c\";\n\
         extern fn unzstd(b: Bytes, max: u64) -> Option<Bytes> = \"blossom_std::compress::zstd_decompress\";\n\
         fn check(b: Bytes) -> bool { crc32c(b) == 0 }\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(ok), Vec::<String>::new());
    let unknown = with_head(
        "extern fn f(b: Bytes) -> u32 = \"blossom_std::checksum::adler32\";\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(unknown), vec!["BLS0216"]);
    let wrong = with_head(
        "extern fn crc(b: Bytes) -> u64 = \"blossom_std::checksum::crc32c\";\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(wrong), vec!["BLS0216"]);
    let table = with_head(
        "extern table fn lines(path: String) -> (n: u64, text: String) = \"blossom_std::io::lines\";\n\
         output out(k: u64);\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert!(codes(table).contains(&"BLS0908".to_owned()), "{:?}", codes(table));
}

#[test]
fn streams_declare_their_relations_and_check_how_they_are_used() {
    let ok = with_head(
        "stream s: listen;\n\
         stream up: connect;\n\
         table last(c: Conn, n: u64) key(c);\n\
         a: on s.data(c, seq, b) { send s.write(c, seq, [Part::Bytes(b)]); upsert last(c, seq); }\n\
         b: on s.closed(c, why) { send s.close(c); }\n\
         d: on go(k, v) { send up.dial(k, \"127.0.0.1:9\"); }\n\
         e: on up.opened(c, req, peer, at) { send up.write(c, 0, [Part::Bytes(Bytes::empty())]); }\n\
         f: on up.failed(req, why) { emit last2(req); }\n\
         table last2(r: u64);\n",
    );
    assert_eq!(codes(ok), Vec::<String>::new());
    let fed = with_head("stream s: listen;\na: on go(k, v) { emit s.closed(Bytes::empty(), \"x\"); }\n");
    assert!(codes(fed).contains(&"BLS0400".to_owned()), "{:?}", codes(fed));
    let read = with_head(
        "stream s: listen;\ntable t(c: Conn);\na: on go(k, v), s.close(c) { emit t(c); }\n",
    );
    assert!(codes(read).contains(&"BLS0203".to_owned()), "{:?}", codes(read));
    let to = with_head("stream s: listen;\na: on s.opened(c, p, at) { send s.close(c) to c; }\n");
    assert_eq!(codes(to), vec!["BLS0403"]);
    let kind = with_head("stream s: accept;\n");
    assert_eq!(codes(kind), vec!["BLS0100"]);
    let twice = with_head("stream s: listen;\nstream s: connect;\n");
    assert_eq!(codes(twice), vec!["BLS0201"]);
    let not_emit = with_head("stream s: listen;\na: on s.opened(c, p, at) { emit s.close(c); }\n");
    assert!(codes(not_emit).contains(&"BLS0400".to_owned()), "{:?}", codes(not_emit));
}

/// Several in-memory files, by path; paths are joined to the including file's directory as given.
struct Files(Vec<(&'static str, &'static str)>);

impl Loader for Files {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        self.0
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(p, text)| LoadedFile {
                key: Arc::from(*p),
                text: (*text).to_owned(),
            })
            .ok_or_else(|| format!("no file `{path}`"))
    }
}

fn codes_of(files: Vec<(&'static str, &'static str)>) -> Vec<String> {
    let mut sources = SourceDb::new();
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let root = files.first().map(|f| f.0).unwrap_or("main.bls");
    match compile(root, &nodes, &mut Files(files), &mut sources) {
        Ok((_, warnings)) => warnings.iter().map(|d| d.code.as_str().to_owned()).collect(),
        Err(BlsError::Rejected(d)) => d.iter().map(|d| d.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn a_textual_include_brings_in_the_files_items_recursively() {
    let files = vec![
        ("main.bls", "program t version 1;\ninclude \"a.bls\";\ninput go(k: u64);\nview v(x) = go(k), let x = twice(inc(k));\n"),
        ("a.bls", "include \"b.bls\";\nfn twice(n: u64) -> u64 { n * 2 }\n"),
        ("b.bls", "fn inc(n: u64) -> u64 { n + 1 }\n"),
    ];
    assert_eq!(codes_of(files), Vec::<String>::new());
}

#[test]
fn an_include_cycle_or_a_missing_file_is_bls0204_and_a_module_include_is_not_implemented() {
    let cycle = vec![
        ("main.bls", "program t version 1;\ninclude \"a.bls\";\n"),
        ("a.bls", "include \"main.bls\";\n"),
    ];
    assert!(codes_of(cycle).contains(&"BLS0204".to_owned()));
    let missing = vec![("main.bls", "program t version 1;\ninclude \"nope.bls\";\n")];
    assert_eq!(codes_of(missing), vec!["BLS0204"]);
    let module = vec![("main.bls", "program t version 1;\ninclude m;\n")];
    assert_eq!(codes_of(module), vec!["BLS0908"]);
}

#[test]
fn block_bodies_in_closures_and_match_arms_and_constant_arms_type_check() {
    let src = with_head(
        "fn f(o: Option<u64>) -> Vec<u64> {\n\
             match o {\n\
                 Some(n) => {\n\
                     let m = n + 1;\n\
                     range(0, m).map(|i| { let j = i * 2; j })\n\
                 }\n\
                 None => [],\n\
             }\n\
         }\n\
         output out(k: u64);\n\
         view w(x) = go(k, _), let x = f(Some(k));\n\
         a: on go(k, v) { emit out(k); }\n",
    );
    assert_eq!(codes(src), Vec::<String>::new());
}
