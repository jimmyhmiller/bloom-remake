use std::sync::Arc;

use blossom_base::{ColIdx, FileId, QualName, RelId, RuleLabel, Span, Symbol, TypeId};
use blossom_ir::build::{FrontendKind, IrBuilder};
use blossom_ir::core::*;
use blossom_ir::obs::FiringKind;
use blossom_value::time::{NodeId, Tick};
use blossom_value::types::IntTy;
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use crate::{Delivery, Instance, Oracle, OracleError, Row, TickInput};

fn span() -> Span {
    Span::point(FileId::from_raw(0), 0)
}

struct B {
    b: IrBuilder,
    int: TypeId,
    node: TypeId,
}

fn int(n: i64) -> Value {
    Value::Int(IntValue::I64(n))
}

fn row(vals: &[Value]) -> Row {
    Arc::from(vals.to_vec())
}

impl B {
    fn new() -> B {
        let meta = ProgramMeta {
            name: Symbol::intern("t"),
            version: 1,
            edition: 1,
            compiler: "t".into(),
            prf_version: 1,
            encoding_version: 1,
            program_id: [0; 16],
        };
        let mut b = IrBuilder::new(meta, FrontendKind::Ded);
        let int = b.types().insert(TypeDef::Int(IntTy::I64)).unwrap();
        let node = b.types().insert(TypeDef::Node(None)).unwrap();
        b.types().insert(TypeDef::Int(IntTy::U64)).unwrap();
        b.types().insert(TypeDef::Bool).unwrap();
        B { b, int, node }
    }

    fn rel(&mut self, name: &str, class: RelClass, tys: &[TypeId]) -> RelId {
        let cols = tys
            .iter()
            .enumerate()
            .map(|(i, t)| Column {
                name: Symbol::intern(&format!("c{i}")),
                ty: *t,
                field_no: None,
                default: None,
                since: None,
                deprecated: None,
                hidden_dest: false,
            })
            .collect::<Vec<_>>();
        let key = (0..cols.len() as u32).map(ColIdx::from_raw).collect();
        self.b
            .declare_relation(RelDecl {
                id: RelId::from_raw(0),
                name: QualName::single(Symbol::intern(name)),
                class,
                schema: Schema {
                    cols,
                    key,
                    payload: vec![],
                    lattice: vec![],
                },
                persistence: Persistence::None,
                durable: false,
                interface: None,
                placement: Placement::Shared,
                origin: Origin::User(span()),
                attrs: RelAttrs {
                    nondet: None,
                    deterministic: false,
                    monotone: false,
                    final_output: false,
                    atomic: false,
                    handler: None,
                    materialize: None,
                    finite: None,
                    range_col: None,
                    partition: None,
                    sealed_by: None,
                },
                span: span(),
            })
            .unwrap()
    }
}

fn atom(rel: RelId, args: Vec<Term>) -> Atom {
    Atom {
        rel,
        args,
        sender: None,
        principal: None,
        weight: None,
        spec: None,
        span: span(),
    }
}

fn head(rel: RelId, args: Vec<Term>) -> Head {
    Head {
        rel,
        args: args.into_iter().map(HeadArg::Term).collect(),
        mode: HeadMode::Insert,
    }
}

fn run(oracle: &Oracle, events: &[(RelId, Row)]) -> Result<crate::TickOutput, OracleError> {
    oracle.tick(&TickInput {
        incarnation: 1,
        node: NodeId(0),
        tick: Tick(1),
        now: blossom_value::time::Instant(0),
        carried: &Instance::default(),
        events,
        delivered: &[],
        ingress: &[],
        capture: true,
    })
}

/// path(x, y) :- edge(x, y); path(x, z) :- path(x, y), edge(y, z); unreached(x) :- node(x), notin path(0, x).
#[test]
fn recursion_and_stratified_negation() {
    let mut b = B::new();
    let edge = b.rel("edge", RelClass::Event(EventSource::Input), &[b.int, b.int]);
    let vertex = b.rel("vertex", RelClass::Event(EventSource::Input), &[b.int]);
    let path = b.rel("path", RelClass::Idb, &[b.int, b.int]);
    let unreached = b.rel("unreached", RelClass::Idb, &[b.int]);
    let zero = b.b.intern_const(int(0)).unwrap();
    {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("p1"), span());
        let (x, y) = (
            r.var(Symbol::intern("X"), b.int).unwrap(),
            r.var(Symbol::intern("Y"), b.int).unwrap(),
        );
        r.lit(Literal::Pos(atom(edge, vec![Term::Var(x), Term::Var(y)])));
        r.head(head(path, vec![Term::Var(x), Term::Var(y)]), None).unwrap();
    }
    {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("p2"), span());
        let x = r.var(Symbol::intern("X"), b.int).unwrap();
        let y = r.var(Symbol::intern("Y"), b.int).unwrap();
        let z = r.var(Symbol::intern("Z"), b.int).unwrap();
        r.lit(Literal::Pos(atom(path, vec![Term::Var(x), Term::Var(y)])));
        r.lit(Literal::Pos(atom(edge, vec![Term::Var(y), Term::Var(z)])));
        r.head(head(path, vec![Term::Var(x), Term::Var(z)]), None).unwrap();
    }
    {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("u"), span());
        let x = r.var(Symbol::intern("X"), b.int).unwrap();
        r.lit(Literal::Pos(atom(vertex, vec![Term::Var(x)])));
        r.lit(Literal::Neg(atom(path, vec![Term::Const(zero), Term::Var(x)])));
        r.head(head(unreached, vec![Term::Var(x)]), None).unwrap();
    }
    let oracle = Oracle::new(b.b.finish().unwrap()).unwrap();
    let strata = oracle.strata();
    let pos = |rel| strata.iter().position(|s| s.rels.contains(&rel)).unwrap();
    assert!(pos(path) < pos(unreached));
    assert!(strata[pos(path)].recursive);

    let mut events = vec![];
    for (a, c) in [(0, 1), (1, 2), (2, 3), (5, 6)] {
        events.push((edge, row(&[int(a), int(c)])));
    }
    for v in 0..7 {
        events.push((vertex, row(&[int(v)])));
    }
    let out = run(&oracle, &events).unwrap();
    let reached: Vec<i64> = out
        .instance
        .rows(path)
        .filter(|r| r[0] == int(0))
        .map(|r| match &r[1] {
            Value::Int(IntValue::I64(n)) => *n,
            other => panic!("not an i64: {other:?}"),
        })
        .collect();
    assert_eq!(reached, [1, 2, 3]);
    let unreached_rows: Vec<Row> = out.instance.rows(unreached).cloned().collect();
    assert_eq!(
        unreached_rows,
        [row(&[int(0)]), row(&[int(4)]), row(&[int(5)]), row(&[int(6)])]
    );
    // path(0, 3) has one firing, through path(0, 2) and edge(2, 3).
    let f: Vec<_> = out
        .firings
        .iter()
        .filter(|f| f.head == row(&[int(0), int(3)]))
        .collect();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].reads.len(), 2);
    // The negated read is recorded with its pattern.
    let neg = out.firings.iter().find(|f| f.head == row(&[int(4)])).unwrap();
    assert_eq!(neg.negations.len(), 1);
    assert_eq!(neg.negations[0].pattern, vec![Some(int(0)), Some(int(4))]);
}

#[test]
fn negation_through_recursion_is_rejected() {
    let mut b = B::new();
    let p = b.rel("p", RelClass::Idb, &[b.int]);
    let q = b.rel("q", RelClass::Event(EventSource::Input), &[b.int]);
    let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("p"), span());
    let x = r.var(Symbol::intern("X"), b.int).unwrap();
    r.lit(Literal::Pos(atom(q, vec![Term::Var(x)])));
    r.lit(Literal::Neg(atom(p, vec![Term::Var(x)])));
    r.head(head(p, vec![Term::Var(x)]), None).unwrap();
    assert!(matches!(
        Oracle::new(b.b.finish().unwrap()),
        Err(OracleError::NotStratifiable(_))
    ));
}

/// total(k, sum<v>), n(k, count<v>), top(k, max<v>) over distinct values; arithmetic overflow is BLSR004.
#[test]
fn aggregates_over_distinct_values_and_checked_arithmetic() {
    let mut b = B::new();
    let u64t = b.b.types().insert(TypeDef::Int(IntTy::U64)).unwrap();
    let kv = b.rel("kv", RelClass::Event(EventSource::Input), &[b.int, b.int, b.int]);
    let total = b.rel("total", RelClass::Idb, &[b.int, b.int]);
    let count = b.rel("n", RelClass::Idb, &[b.int, u64t]);
    let top = b.rel("top", RelClass::Idb, &[b.int, b.int]);
    let plus = b.rel("plus", RelClass::Idb, &[b.int]);
    for (target, func, label) in [
        (total, AggFunc::Sum, "s"),
        (count, AggFunc::Count, "c"),
        (top, AggFunc::Max, "m"),
    ] {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new(label), span());
        let k = r.var(Symbol::intern("K"), b.int).unwrap();
        let v = r.var(Symbol::intern("V"), b.int).unwrap();
        r.lit(Literal::Pos(atom(kv, vec![Term::Var(k), Term::Var(v), Term::Wild])));
        r.head(
            Head {
                rel: target,
                args: vec![
                    HeadArg::Term(Term::Var(k)),
                    HeadArg::Agg(AggCall {
                        func,
                        args: vec![Term::Var(v)],
                        order: None,
                    }),
                ],
                mode: HeadMode::Insert,
            },
            None,
        )
        .unwrap();
    }
    {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("plus"), span());
        let s = r.var(Symbol::intern("S"), b.int).unwrap();
        let t = r.var(Symbol::intern("T"), b.int).unwrap();
        r.lit(Literal::Pos(atom(total, vec![Term::Wild, Term::Var(s)])));
        r.lit(Literal::Bind {
            pat: Pattern::Var(t),
            expr: Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(Expr::Term(Term::Var(s))),
                rhs: Box::new(Expr::Term(Term::Var(s))),
            },
        });
        r.head(head(plus, vec![Term::Var(t)]), None).unwrap();
    }
    let oracle = Oracle::new(b.b.finish().unwrap()).unwrap();
    // Two valuations with the value 5 for key 1 count once (set semantics over distinct values).
    let events = vec![
        (kv, row(&[int(1), int(5), int(0)])),
        (kv, row(&[int(1), int(5), int(1)])),
        (kv, row(&[int(1), int(7), int(0)])),
        (kv, row(&[int(2), int(3), int(0)])),
    ];
    let out = run(&oracle, &events).unwrap();
    assert!(out.instance.contains(total, &[int(1), int(12)]));
    assert!(out.instance.contains(count, &[int(1), Value::Int(IntValue::U64(2))]));
    assert!(out.instance.contains(top, &[int(1), int(7)]));
    assert!(out.instance.contains(plus, &[int(144)]));
    let agg = out
        .firings
        .iter()
        .find(|f| f.kind == FiringKind::Aggregate && f.head == row(&[int(1), int(12)]))
        .unwrap();
    assert_eq!(agg.reads.len(), 3, "every contributing valuation's reads");

    let big = vec![(kv, row(&[int(1), int(i64::MAX / 2), int(0)]))];
    match run(&oracle, &big) {
        Err(OracleError::Program { error, .. }) => assert_eq!(error.code, "BLSR004"),
        other => panic!("expected BLSR004, got {other:?}"),
    }
}

/// counter(x+1)@next :- counter(x); ping(dest, x)@async :- counter(x), peer(dest); got(x) :- ping(_, x).
#[test]
fn next_heads_carry_and_async_heads_send() {
    let mut b = B::new();
    let counter = b.rel("counter", RelClass::Idb, &[b.int]);
    let peer = b.rel("peer", RelClass::Event(EventSource::Input), &[b.node]);
    let ping = b.rel(
        "ping",
        RelClass::Channel(ChannelDecl {
            form: ChannelForm::NodeToNode,
            loopback: false,
            host_endpoint: false,
            fault: FaultModel::Lossy,
            partition: None,
            sealed_by: None,
            wrapper: None,
            acl: AclSpec::Inferred,
            egress_to_external: false,
            replicated: false,
        }),
        &[b.node, b.int],
    );
    let got = b.rel("got", RelClass::Idb, &[b.int]);
    let one = b.b.intern_const(int(1)).unwrap();
    {
        let mut r = b.b.rule(RuleKind::Inductive, RuleLabel::new("inc"), span());
        let x = r.var(Symbol::intern("X"), b.int).unwrap();
        let y = r.var(Symbol::intern("Y"), b.int).unwrap();
        r.lit(Literal::Pos(atom(counter, vec![Term::Var(x)])));
        r.lit(Literal::Bind {
            pat: Pattern::Var(y),
            expr: Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Term(Term::Var(x))),
                rhs: Box::new(Expr::Term(Term::Const(one))),
            },
        });
        r.head(head(counter, vec![Term::Var(y)]), None).unwrap();
    }
    {
        let mut r = b.b.rule(RuleKind::Async, RuleLabel::new("send"), span());
        let x = r.var(Symbol::intern("X"), b.int).unwrap();
        let d = r.var(Symbol::intern("D"), b.node).unwrap();
        r.lit(Literal::Pos(atom(counter, vec![Term::Var(x)])));
        r.lit(Literal::Pos(atom(peer, vec![Term::Var(d)])));
        r.head(head(ping, vec![Term::Var(d), Term::Var(x)]), None).unwrap();
    }
    {
        let mut r = b.b.rule(RuleKind::Deductive, RuleLabel::new("recv"), span());
        let x = r.var(Symbol::intern("X"), b.int).unwrap();
        r.lit(Literal::Pos(atom(ping, vec![Term::Wild, Term::Var(x)])));
        r.head(head(got, vec![Term::Var(x)]), None).unwrap();
    }
    let oracle = Oracle::new(b.b.finish().unwrap()).unwrap();
    let mut carried = Instance::default();
    carried.insert(counter, row(&[int(4)]));
    let events = vec![(peer, row(&[Value::Node(NodeId(1))]))];
    let delivered = vec![Delivery {
        rel: ping,
        from: NodeId(1),
        row: row(&[Value::Node(NodeId(0)), int(9)]),
    }];
    let out = oracle
        .tick(&TickInput {
            incarnation: 1,
            node: NodeId(0),
            tick: Tick(3),
            now: blossom_value::time::Instant(0),
            carried: &carried,
            events: &events,
            delivered: &delivered,
            ingress: &[],
            capture: false,
        })
        .unwrap();
    assert!(out.next.contains(counter, &[int(5)]));
    assert!(!out.next.contains(counter, &[int(4)]), "no implicit persistence");
    let sends: Vec<_> = out.outbox.iter().map(|s| (s.to, s.row.clone())).collect();
    assert_eq!(sends, [(NodeId(1), row(&[Value::Node(NodeId(1)), int(4)]))]);
    assert!(out.instance.contains(got, &[int(9)]));
    assert!(out.firings.is_empty(), "no capture requested");
}
