use std::path::{Path, PathBuf};

use blossom_base::{FileId, Symbol};

use super::*;

fn file() -> FileId {
    FileId::from_raw(0)
}

fn parse_ok(text: &str) -> DedFile {
    let (f, d) = parse(file(), text);
    assert!(
        d.is_empty(),
        "unexpected diagnostics: {:?}",
        d.iter().map(|d| d.to_string()).collect::<Vec<_>>()
    );
    f
}

fn codes(text: &str) -> Vec<String> {
    let (_, d) = parse(file(), text);
    d.iter().map(|d| d.code.as_str().to_owned()).collect()
}

fn rule(c: &Clause) -> &Rule {
    match c {
        Clause::Rule(r) => r,
        other => panic!("not a rule: {other:?}"),
    }
}

#[test]
fn simple_deliv_parses() {
    let f = parse_ok(
        "log(Node, Pload) :- bcast(Node, Pload);\n\
         node(Node, Neighbor)@next :- node(Node, Neighbor);\n\
         log(Node2, Pload)@async :- bcast(Node1, Pload), node(Node1, Node2);\n\
         node(\"a\", \"b\")@1;\n\
         include \"group.ded\";",
    );
    assert_eq!(f.clauses.len(), 5);
    assert_eq!(rule(&f.clauses[0]).time, HeadTime::Now);
    assert_eq!(rule(&f.clauses[1]).time, HeadTime::Next);
    let r = rule(&f.clauses[2]);
    assert_eq!(r.time, HeadTime::Async);
    assert_eq!(r.body.len(), 2);
    match &f.clauses[3] {
        Clause::Fact(fact) => {
            assert_eq!(fact.rel.text, Symbol::intern("node"));
            assert_eq!(fact.time, 1);
            assert!(matches!(&fact.args[..], [Term::Str(a, _), Term::Str(b, _)] if &**a == "a" && &**b == "b"));
        }
        other => panic!("not a fact: {other:?}"),
    }
    assert!(matches!(&f.clauses[4], Clause::Include(i) if &*i.path == "group.ded"));
}

#[test]
fn comments_of_every_form_are_skipped() {
    let f = parse_ok("// line\n# hash\n/* block\n over lines */ p(X) :- q(X); # trailing\n");
    assert_eq!(f.clauses.len(), 1);
}

#[test]
fn expressions_nest_to_the_right_without_precedence() {
    let f = parse_ok("p(X, Y - Z + 1) :- q(X, Y, Z), X == Y * 2;");
    let r = rule(&f.clauses[0]);
    // Y - Z + 1 parses as Y - (Z + 1).
    match &r.head.args[1] {
        Arg::Expr(Expr::Binary {
            lhs: Term::Var(y),
            op: BinOp::Sub,
            rhs,
            ..
        }) => {
            assert_eq!(y.text, Symbol::intern("Y"));
            assert!(matches!(
                &**rhs,
                Expr::Binary {
                    lhs: Term::Var(_),
                    op: BinOp::Add,
                    rhs: _,
                    ..
                }
            ));
        }
        other => panic!("unexpected head argument {other:?}"),
    }
    assert!(matches!(&r.body[1], BodyItem::Qual(Expr::Binary { op: BinOp::Eq, .. })));
}

#[test]
fn aggregates_negation_and_absolute_times() {
    let f = parse_ok("votes(C, T, count<M>) :- tally(C, T, M), notin crash(C, C, _), begin(C, X)@1;");
    let r = rule(&f.clauses[0]);
    assert!(matches!(
        &r.head.args[2],
        Arg::Agg(Aggregate {
            func: AggFunc::Count,
            ..
        })
    ));
    assert!(matches!(&r.body[1], BodyItem::Atom(BodyAtom { negated: true, .. })));
    assert!(matches!(
        &r.body[2],
        BodyItem::Atom(BodyAtom {
            time: Some(1),
            negated: false,
            ..
        })
    ));
    // `count` not followed by `<Var>` is an ordinary name.
    parse_ok("count(X) :- q(X);");
}

#[test]
fn comparison_operators_are_not_aggregate_brackets() {
    let f = parse_ok("p(X) :- q(X, Y), X < Y, Y >= 3;");
    assert_eq!(rule(&f.clauses[0]).body.len(), 3);
}

#[test]
fn malformed_clauses_are_reported_and_skipped() {
    assert_eq!(codes("p(X) :- q(X)"), ["BLS0100"]);
    assert_eq!(codes("p(x) :- q(X);"), ["BLS0100"]);
    assert_eq!(codes("p(X)@3 :- q(X);"), ["BLS0100"]);
    assert_eq!(codes("p(\"a\");"), ["BLS0100"]);
    assert_eq!(codes("p(X)@1;"), ["BLS0100"]);
    assert_eq!(codes("notin p(X) :- q(X);"), ["BLS0100"]);
    assert_eq!(codes("p(X) :- q(X)@next;"), ["BLS0100"]);
    assert_eq!(codes("p(X) :- q(X), X + 1;"), ["BLS0100"]);
    assert_eq!(codes("p(X) :- q(X), _ == X;"), ["BLS0100"]);
    assert_eq!(codes("p(X) :- q(X), X == _;"), ["BLS0100"]);
    assert_eq!(codes("p(99999999999999999999)@1;"), ["BLS0100"]);
    assert_eq!(codes("p(1) :- q(X), sum<x> == 1;"), ["BLS0100"]);
    // Recovery continues after the bad clause.
    let (f, d) = parse(file(), "p(X :- q(X); r(X) :- s(X);");
    assert_eq!(d.len(), 1);
    assert_eq!(f.clauses.len(), 1);
    // A missing `;` does not swallow the clause on the next line.
    let (f, d) = parse(file(), "p(X) :- q(X)\nr(X) :- s(X);\nt(X) :- u(X);");
    assert_eq!(d.len(), 1);
    assert_eq!(f.clauses.len(), 2);
}

#[test]
fn lexical_errors_have_their_codes() {
    assert_eq!(codes("p(X) :- q(X) & r(X);"), ["BLS0001", "BLS0100"]);
    assert_eq!(codes("p(\"abc) :- q(X);"), ["BLS0002", "BLS0100"]);
    assert!(codes("p(X) :- q(X); /* never closed").contains(&"BLS0002".to_owned()));
}

fn corpus_ded_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            corpus_ded_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ded") {
            out.push(path);
        }
    }
}

#[test]
fn every_corpus_ded_file_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let mut files = Vec::new();
    corpus_ded_files(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .ded files", files.len());
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let (_, d) = parse(file(), &text);
        assert!(
            d.is_empty(),
            "{}: {:?}",
            path.display(),
            d.iter().map(|d| d.to_string()).collect::<Vec<_>>()
        );
    }
}
