use crate::{
    ValidatedProgram,
    build::{FrontendKind, IrBuilder},
    core::*,
};
use blossom_base::{QualName, RuleLabel, Span, Symbol, idx::*};
use blossom_value::{TypeDef, types::IntTy};
fn s(x: &str) -> Symbol {
    Symbol::intern(x)
}
fn name(x: &str) -> QualName {
    QualName::parse_dotted(x).unwrap()
}
fn span() -> Span {
    Span::new(FileId::from_raw(0), 0, 1)
}
fn meta() -> ProgramMeta {
    ProgramMeta {
        name: s("test"),
        version: 1,
        edition: 1,
        compiler: "test".into(),
        prf_version: 1,
        encoding_version: 1,
        program_id: [0; 16],
    }
}
fn attrs() -> RelAttrs {
    RelAttrs {
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
    }
}
fn rel(id: u32, name_: &str, ty: TypeId, class: RelClass) -> RelDecl {
    RelDecl {
        id: RelId::from_raw(id),
        name: name(name_),
        class,
        schema: Schema {
            cols: vec![Column {
                name: s("x"),
                ty,
                field_no: None,
                default: None,
                since: None,
                deprecated: None,
                hidden_dest: false,
            }],
            key: vec![ColIdx::from_raw(0)],
            payload: vec![],
            lattice: vec![],
        },
        persistence: Persistence::None,
        durable: false,
        interface: None,
        placement: Placement::Shared,
        origin: Origin::User(span()),
        attrs: attrs(),
        span: span(),
    }
}
fn base() -> Program {
    let mut p = Program::new(meta());
    let ty = p.types.insert(TypeDef::Int(IntTy::U64)).unwrap();
    p.rels.push(rel(0, "r", ty, RelClass::Idb)).unwrap();
    p.rels.push(rel(1, "s", ty, RelClass::Static)).unwrap();
    p.rules
        .push(Rule {
            id: RuleId::from_raw(0),
            label: RuleLabel::new("r/from_s"),
            kind: RuleKind::Deductive,
            head: Head {
                rel: RelId::from_raw(0),
                args: vec![HeadArg::Term(Term::Var(VarId::from_raw(0)))],
                mode: HeadMode::Insert,
            },
            body: Body {
                vars: IndexVec::try_from_iter([VarDecl {
                    name: s("X"),
                    ty,
                    non_bottom: false,
                }])
                .unwrap(),
                lits: vec![Literal::Pos(Atom {
                    rel: RelId::from_raw(1),
                    args: vec![Term::Var(VarId::from_raw(0))],
                    sender: None,
                    principal: None,
                    weight: None,
                    spec: None,
                    span: span(),
                })],
            },
            role: None,
            construct: None,
            span: span(),
        })
        .unwrap();
    p
}
fn has(p: Program, v: u8) -> bool {
    crate::validate::validate(&p).iter().any(|e| e.invariant() == Some(v))
}
fn good() -> Program {
    let p = base();
    assert!(ValidatedProgram::validate(p.clone()).is_ok());
    p
}
#[test]
fn validator_v1_accept_and_reject() {
    let mut p = good();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args[0] = HeadArg::Term(Term::Var(VarId::from_raw(1)));
    assert!(has(p, 1));
}
#[test]
fn validator_v2_accept_and_reject() {
    let mut p = good();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().kind = RuleKind::Async;
    assert!(has(p, 2));
}
#[test]
fn validator_v3_accept_and_reject() {
    let mut p = good();
    p.rels.get_mut(RelId::from_raw(1)).unwrap().placement = Placement::Role(RoleId::from_raw(0));
    assert!(has(p, 3));
}
#[test]
fn validator_v4_accept_and_reject() {
    let mut p = good();
    p.rels
        .get_mut(RelId::from_raw(0))
        .unwrap()
        .schema
        .payload
        .push(ColIdx::from_raw(0));
    assert!(has(p, 4));
}
#[test]
fn validator_v5_accept_and_reject() {
    let mut p = good();
    p.rels.get_mut(RelId::from_raw(0)).unwrap().persistence = Persistence::Identity {
        rule: RuleId::from_raw(0),
    };
    assert!(has(p, 5));
}
#[test]
fn validator_v6_accept_and_reject() {
    let mut p = good();
    p.rels.get_mut(RelId::from_raw(0)).unwrap().name = name("bad$name");
    assert!(has(p, 6));
}
#[test]
fn validator_v7_accept_and_reject() {
    let mut p = good();
    let id = p
        .constructs
        .push(Construct {
            id: ConstructId::from_raw(0),
            kind: ConstructKind::Outer,
            rules: vec![],
            rels: vec![],
            surface: SurfaceRef {
                module: name("test"),
                label: None,
                stmt: None,
                span: span(),
            },
        })
        .unwrap();
    p.sites
        .push(Site {
            id: SiteId::from_raw(0),
            stable: "".into(),
            key: 0,
            kind: SiteKind::Choose,
            construct: id,
        })
        .unwrap();
    assert!(has(p, 7));
}
#[test]
fn validator_v8_accept_and_reject() {
    let mut p = good();
    let bool_ty = p.types.insert(TypeDef::Bool).unwrap();
    p.rules
        .get_mut(RuleId::from_raw(0))
        .unwrap()
        .body
        .vars
        .get_mut(VarId::from_raw(0))
        .unwrap()
        .ty = bool_ty;
    assert!(has(p, 8));
}
#[test]
fn validator_v8_aggregate_signatures() {
    let mut p = good();
    let var = Term::Var(VarId::from_raw(0));
    let rule = p.rules.get_mut(RuleId::from_raw(0)).unwrap();
    // A count over a tuple is accepted (LANGUAGE §10.1); a wildcard in the counted tuple is not.
    rule.head.args[0] = HeadArg::Agg(AggCall {
        func: AggFunc::Count,
        args: vec![var.clone(), Term::Wild],
        order: None,
    });
    assert!(has(p.clone(), 8));
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args[0] = HeadArg::Agg(AggCall {
        func: AggFunc::Count,
        args: vec![var.clone(), var.clone()],
        order: None,
    });
    assert!(!has(p.clone(), 8));
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args[0] = HeadArg::Agg(AggCall {
        func: AggFunc::Count,
        args: vec![var.clone()],
        order: None,
    });
    assert!(!has(p.clone(), 8));
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args[0] = HeadArg::Agg(AggCall {
        func: AggFunc::BoolAnd,
        args: vec![var],
        order: None,
    });
    assert!(has(p, 8));
}
#[test]
fn validator_v8_rejects_head_wildcard() {
    let mut p = good();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args[0] = HeadArg::Term(Term::Wild);
    assert!(has(p, 8));
}
#[test]
fn validator_v8_uses_context_for_node_constant() {
    let mut p = good();
    let role = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(0),
            name: name("A"),
            kind: RoleKind::Process,
        })
        .unwrap();
    let _generic = p.types.insert(TypeDef::Node(None)).unwrap();
    let specific = p.types.insert(TypeDef::Node(Some(role))).unwrap();
    p.types.insert(TypeDef::Bool).unwrap();
    p.rels.push(rel(2, "nodes", specific, RelClass::Static)).unwrap();
    let constant = p
        .consts
        .push(blossom_value::Value::Node(blossom_value::NodeId(1)))
        .unwrap();
    let rule = p.rules.get_mut(RuleId::from_raw(0)).unwrap();
    let node_var = rule
        .body
        .vars
        .push(VarDecl {
            name: s("N"),
            ty: specific,
            non_bottom: false,
        })
        .unwrap();
    rule.body.lits.push(Literal::Pos(Atom {
        rel: RelId::from_raw(2),
        args: vec![Term::Var(node_var)],
        sender: None,
        principal: None,
        weight: None,
        spec: None,
        span: span(),
    }));
    rule.body.lits.push(Literal::Guard(Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(Expr::Term(Term::Var(node_var))),
        rhs: Box::new(Expr::Term(Term::Const(constant))),
    }));
    assert!(ValidatedProgram::validate(p).is_ok());
}
#[test]
fn validator_v8_rejects_non_lattice_majority_and_non_weighted_zweight() {
    let mut p = good();
    p.types.insert(TypeDef::Bool).unwrap();
    p.roles
        .push(RoleDecl {
            id: RoleId::from_raw(0),
            name: name("A"),
            kind: RoleKind::Process,
        })
        .unwrap();
    let arg = Expr::Term(Term::Var(VarId::from_raw(0)));
    let rule = p.rules.get_mut(RuleId::from_raw(0)).unwrap();
    rule.body.lits.push(Literal::Guard(Expr::Call {
        f: FnRef::Builtin(BuiltinFn::Majority {
            domain: MajorityDomain::Role(RoleId::from_raw(0)),
        }),
        args: vec![arg.clone()],
    }));
    assert!(has(p.clone(), 8));
    let guard = p
        .rules
        .get_mut(RuleId::from_raw(0))
        .unwrap()
        .body
        .lits
        .last_mut()
        .unwrap();
    *guard = Literal::Bind {
        pat: Pattern::Var(VarId::from_raw(0)),
        expr: Expr::Call {
            f: FnRef::Builtin(BuiltinFn::ZWeight {
                rel: RelId::from_raw(0),
            }),
            args: vec![arg],
        },
    };
    assert!(has(p, 8));
}
#[test]
fn validator_v8_polymorphic_builtin_requires_monomorphic_signature() {
    let mut p = good();
    let str_ty = p.types.insert(TypeDef::Str).unwrap();
    let u = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
    let message = p.consts.push(blossom_value::Value::Str("bad input".into())).unwrap();
    p.rules
        .get_mut(RuleId::from_raw(0))
        .unwrap()
        .body
        .lits
        .push(Literal::Bind {
            pat: Pattern::Var(VarId::from_raw(0)),
            expr: Expr::Call {
                f: FnRef::Builtin(BuiltinFn::Entries),
                args: vec![Expr::Term(Term::Const(message))],
            },
        });
    assert!(has(p.clone(), 8));
    p.fns
        .push(FnDecl {
            id: FnId::from_raw(0),
            name: name("entries_u64"),
            params: vec![(s("message"), str_ty)],
            ret: u,
            vars: IndexVec::new(),
            body: FnBody::Builtin(BuiltinFn::Entries),
            props: FnProps {
                classes: vec![],
                injective: Claim::Absent,
                commutative: Claim::Absent,
                associative: Claim::Absent,
                idempotent: Claim::Absent,
                stable_after: None,
                metered: true,
            },
        })
        .unwrap();
    assert!(!has(p, 8));
}
#[test]
fn validator_v9_accept_and_reject() {
    let mut p = good();
    let node_ty = p.types.insert(TypeDef::Node(None)).unwrap();
    let group_ty = p.types.insert(TypeDef::Group(GroupTypeId::from_raw(0))).unwrap();
    p.groups
        .push(GroupDef {
            id: GroupTypeId::from_raw(0),
            ctor: GroupCtor::Z,
            ring: false,
        })
        .unwrap();
    let mut c = rel(
        2,
        "c",
        node_ty,
        RelClass::Channel(ChannelDecl {
            form: ChannelForm::Column,
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
    );
    c.schema.cols.push(Column {
        name: s("g"),
        ty: group_ty,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    });
    c.schema.key.push(ColIdx::from_raw(1));
    p.rels.push(c).unwrap();
    assert!(has(p, 9));
}
#[test]
fn validator_v10_accept_and_reject() {
    let mut p = good();
    let mut r = p.rules.get(RuleId::from_raw(0)).unwrap().clone();
    r.id = RuleId::from_raw(1);
    p.rules.push(r).unwrap();
    assert!(has(p, 10));
}
#[test]
fn validator_v11_accept_and_reject() {
    let mut p = good();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.args.clear();
    assert!(has(p, 11));
}
#[test]
fn validator_v12_accept_and_reject() {
    let mut p = good();
    if let Literal::Pos(a) = &mut p.rules.get_mut(RuleId::from_raw(0)).unwrap().body.lits[0] {
        a.spec = Some(SpecAt {
            loc: Term::Wild,
            time: SpecTime::Eval,
        });
    }
    assert!(has(p, 12));
}
#[test]
fn digest_ignores_spans() {
    let p = good();
    let a = ValidatedProgram::validate(p.clone()).unwrap().digest();
    let mut q = p;
    q.rules.get_mut(RuleId::from_raw(0)).unwrap().span = Span::new(FileId::from_raw(3), 100, 200);
    q.rels.get_mut(RelId::from_raw(0)).unwrap().span = Span::new(FileId::from_raw(6), 30, 40);
    assert_eq!(a, ValidatedProgram::validate(q).unwrap().digest());
}
#[test]
fn digest_invariant_under_relation_renumbering() {
    let p = good();
    let a = ValidatedProgram::validate(p.clone()).unwrap().digest();
    let mut q = p;
    q.rels.as_mut_slice().swap(0, 1);
    for (id, r) in q.rels.iter_enumerated_mut() {
        r.id = id;
    }
    q.rules.get_mut(RuleId::from_raw(0)).unwrap().head.rel = RelId::from_raw(1);
    if let Literal::Pos(atom) = &mut q.rules.get_mut(RuleId::from_raw(0)).unwrap().body.lits[0] {
        atom.rel = RelId::from_raw(0)
    }
    assert_eq!(a, ValidatedProgram::validate(q).unwrap().digest());
}
#[test]
fn postcard_roundtrip_program() {
    let p = good();
    let bytes = postcard::to_allocvec(&p).unwrap();
    let decoded: Program = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(p, decoded);
}
#[test]
fn builder_construct_membership() {
    let mut b = IrBuilder::new(meta(), FrontendKind::Blossom);
    let ty = b.types().insert(TypeDef::Int(IntTy::U64)).unwrap();
    let id = b
        .begin_construct(
            ConstructKind::Outer,
            SurfaceRef {
                module: name("test"),
                label: None,
                stmt: None,
                span: span(),
            },
        )
        .unwrap();
    let mut d = rel(0, "r$generated", ty, RelClass::Idb);
    d.origin = Origin::Generated { construct: id };
    let rel = b.declare_relation(d).unwrap();
    b.end_construct(id).unwrap();
    let p = b.finish().unwrap();
    assert_eq!(p.get().constructs.get(id).unwrap().rels, vec![rel]);
}
#[test]
fn time_varying_scalars() {
    assert!(Expr::Scalar(BuiltinScalar::Now).time_varying());
    assert!(Expr::Scalar(BuiltinScalar::Tick).time_varying());
    assert!(!Expr::Scalar(BuiltinScalar::SelfNode).time_varying());
    assert!(
        Expr::If {
            cond: Box::new(Expr::Term(Term::Wild)),
            then: Box::new(Expr::Scalar(BuiltinScalar::Incarnation)),
            els: Box::new(Expr::Term(Term::Wild))
        }
        .time_varying()
    );
}
#[test]
fn printer_basic() {
    let p = good();
    let text = crate::printer::print(&p);
    assert!(text.contains("r(X) :- s(X)."));
    insta::assert_snapshot!(text);
}
#[test]
fn printer_schema_annotations() {
    let mut p = base();
    let u = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
    let lattice = p
        .lattices
        .push(LatticeDef {
            id: LatticeTypeId::from_raw(0),
            name: name("LMax"),
            ctor: LatticeCtor::Max(u),
            ops: vec![],
            height: HeightClass::Acc,
            laws: LawStatus::Builtin,
            distributive: true,
            dense_domain: None,
        })
        .unwrap();
    let lat_ty = p.types.insert(TypeDef::Lattice(lattice)).unwrap();
    let mut cell = rel(2, "votes$now", u, RelClass::Idb);
    cell.schema.cols.push(Column {
        name: s("count"),
        ty: lat_ty,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    });
    cell.schema.lattice.push((ColIdx::from_raw(1), lattice));
    p.rels.push(cell).unwrap();
    let mut payload = rel(3, "kv", u, RelClass::Idb);
    payload.schema.cols.push(Column {
        name: s("value"),
        ty: u,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    });
    payload.schema.payload.push(ColIdx::from_raw(1));
    payload.durable = true;
    p.rels.push(payload).unwrap();
    for n in ["A", "B"] {
        p.roles
            .push(RoleDecl {
                id: RoleId::from_raw(p.roles.len() as u32),
                name: name(n),
                kind: RoleKind::Process,
            })
            .unwrap();
    }
    let node = p.types.insert(TypeDef::Node(None)).unwrap();
    let mut channel = rel(
        4,
        "vote",
        node,
        RelClass::Channel(ChannelDecl {
            form: ChannelForm::Direction {
                src: RoleId::from_raw(0),
                dst: RoleId::from_raw(1),
            },
            loopback: false,
            host_endpoint: false,
            fault: FaultModel::ReliableOrdered,
            partition: None,
            sealed_by: None,
            wrapper: None,
            acl: AclSpec::Inferred,
            egress_to_external: false,
            replicated: false,
        }),
    );
    channel.schema.cols[0].name = s("dest");
    channel.schema.cols[0].hidden_dest = true;
    channel.schema.cols.push(Column {
        name: s("term"),
        ty: u,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    });
    channel.schema.key.push(ColIdx::from_raw(1));
    p.rels.push(channel).unwrap();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().body.lits[0] = Literal::Pos(Atom {
        rel: RelId::from_raw(4),
        args: vec![Term::Wild, Term::Var(VarId::from_raw(0))],
        sender: None,
        principal: Some(Term::Var(VarId::from_raw(0))),
        weight: None,
        spec: None,
        span: span(),
    });
    let text = crate::printer::print(&p);
    assert!(text.contains("vote(X | X)"));
    insta::assert_snapshot!(text);
}
#[test]
fn project_role_basic() {
    let mut p = good();
    let a = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(0),
            name: name("A"),
            kind: RoleKind::Process,
        })
        .unwrap();
    let b = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(1),
            name: name("B"),
            kind: RoleKind::Process,
        })
        .unwrap();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().role = Some(a);
    let mut other = p.rules.get(RuleId::from_raw(0)).unwrap().clone();
    other.id = RuleId::from_raw(1);
    other.role = Some(b);
    other.label = RuleLabel::new("b/other");
    p.rules.push(other).unwrap();
    let valid = ValidatedProgram::validate(p).unwrap();
    let projection = valid.project(a).unwrap();
    assert_eq!(projection.get().rules.len(), 1);
    assert_eq!(projection.get().rules.get(RuleId::from_raw(0)).unwrap().role, None);
}

#[test]
fn digest_invariant_under_type_and_constant_order() {
    let mut a = base();
    a.types.insert(TypeDef::Bool).unwrap();
    let used = a.consts.push(blossom_value::Value::u64(1)).unwrap();
    let _unused = a.consts.push(blossom_value::Value::u64(9)).unwrap();
    a.facts.push(Fact {
        rel: RelId::from_raw(1),
        row: vec![used],
        span: span(),
    });
    let ca = crate::canonical::canonical(&a).unwrap();
    let first = ValidatedProgram::validate(a).unwrap().digest();
    let mut b = Program::new(meta());
    b.types.insert(TypeDef::Bool).unwrap();
    let u = b.types.insert(TypeDef::Int(IntTy::U64)).unwrap();
    b.rels.push(rel(0, "r", u, RelClass::Idb)).unwrap();
    b.rels.push(rel(1, "s", u, RelClass::Static)).unwrap();
    let mut rule = base().rules.get(RuleId::from_raw(0)).unwrap().clone();
    rule.body.vars.get_mut(VarId::from_raw(0)).unwrap().ty = u;
    b.rules.push(rule).unwrap();
    let _unused = b.consts.push(blossom_value::Value::u64(9)).unwrap();
    let used = b.consts.push(blossom_value::Value::u64(1)).unwrap();
    b.facts.push(Fact {
        rel: RelId::from_raw(1),
        row: vec![used],
        span: span(),
    });
    let cb = crate::canonical::canonical(&b).unwrap();
    assert_eq!(ca, cb);
    assert_eq!(first, ValidatedProgram::validate(b).unwrap().digest());
}

#[test]
fn postcard_roundtrip_spec_and_plan() {
    use crate::{plan::*, spec::*};
    let digest = ValidatedProgram::validate(base()).unwrap().digest();
    let spec = SpecProgram {
        name: name("test_spec"),
        target: digest,
        nodes: vec![],
        assign: vec![],
        faults: None,
        facts: vec![],
        trace_rels: IndexVec::new(),
        rules: IndexVec::new(),
        constructs: IndexVec::new(),
        pre: None,
        post: None,
        invariants: vec![],
        liveness: vec![],
        proofs: vec![],
        expects: vec![],
        checks: vec![],
    };
    let bytes = postcard::to_allocvec(&spec).unwrap();
    assert_eq!(postcard::from_bytes::<SpecProgram>(&bytes).unwrap(), spec);
    let plan = PhysicalProgram {
        program: digest,
        role: None,
        planner_version: 1,
        abi: 1,
        profile: PlanProfile::Literal,
        features: vec![],
        rels: IndexVec::new(),
        strata: IndexVec::new(),
        temporal: TemporalPlan {
            inductive: vec![],
            asynchronous: vec![],
        },
        natives: IndexVec::new(),
        agg_tables: IndexVec::new(),
        buffers: IndexVec::new(),
        ingest: IngestPlan {
            deliveries: vec![],
            timers: vec![],
            host_inputs: vec![],
            boot: None,
            recovered: None,
            table_fns: vec![],
            branching: vec![],
        },
        prov: ProvPlan {
            tier: ProvTier::Off,
            rules: vec![],
            annotation_cols: vec![],
        },
        digests: DigestPlan {
            state: true,
            outbox: true,
            choices: true,
            changed: vec![],
        },
        empty_tick_effects: false,
        limits: PlanLimits {
            iterations: 100,
            batch_size: 1024,
            fuel: 1_000_000,
            max_group: 1024,
            natives: false,
            elastic_threshold: None,
        },
    };
    let bytes = postcard::to_allocvec(&plan).unwrap();
    assert_eq!(postcard::from_bytes::<PhysicalProgram>(&bytes).unwrap(), plan);
}

#[test]
fn digest_invariant_under_rule_and_variable_renumbering() {
    let mut p = base();
    let ty = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
    for (_, r) in p.rels.iter_enumerated_mut() {
        r.schema.cols.push(Column {
            name: s("y"),
            ty,
            field_no: None,
            default: None,
            since: None,
            deprecated: None,
            hidden_dest: false,
        });
        r.schema.key.push(ColIdx::from_raw(1));
    }
    let rule = p.rules.get_mut(RuleId::from_raw(0)).unwrap();
    rule.body
        .vars
        .push(VarDecl {
            name: s("Y"),
            ty,
            non_bottom: false,
        })
        .unwrap();
    rule.head.args.push(HeadArg::Term(Term::Var(VarId::from_raw(1))));
    if let Literal::Pos(a) = &mut rule.body.lits[0] {
        a.args.push(Term::Var(VarId::from_raw(1)));
    }
    let mut second = rule.clone();
    second.id = RuleId::from_raw(1);
    second.label = RuleLabel::new("r/another");
    p.rules.push(second).unwrap();
    let first = ValidatedProgram::validate(p.clone()).unwrap().digest();
    let mut q = p;
    q.rules.as_mut_slice().swap(0, 1);
    for (id, r) in q.rules.iter_enumerated_mut() {
        r.id = id;
        r.body.vars.as_mut_slice().swap(0, 1);
        for t in r.head.args.iter_mut() {
            if let HeadArg::Term(Term::Var(v)) = t {
                *v = VarId::from_raw(1 - v.raw())
            }
        }
        for l in &mut r.body.lits {
            if let Literal::Pos(a) = l {
                for t in &mut a.args {
                    if let Term::Var(v) = t {
                        *v = VarId::from_raw(1 - v.raw())
                    }
                }
            }
        }
    }
    assert_eq!(first, ValidatedProgram::validate(q).unwrap().digest());
}

fn assert_digest_permutation(flags: [bool; 15]) {
    use crate::visit::{Mapper, Remap};
    let mut p = base();
    let u = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
    let _bool_ty = p.types.insert(TypeDef::Bool).unwrap();
    p.consts.push(blossom_value::Value::u64(1)).unwrap();
    p.consts.push(blossom_value::Value::u64(2)).unwrap();
    p.facts.push(Fact {
        rel: RelId::from_raw(1),
        row: vec![ConstId::from_raw(0)],
        span: span(),
    });
    for (i, n) in ["A", "B"].iter().enumerate() {
        p.roles
            .push(RoleDecl {
                id: RoleId::from_raw(i as u32),
                name: name(n),
                kind: RoleKind::Process,
            })
            .unwrap();
    }
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().role = Some(RoleId::from_raw(0));
    let mut second = p.rules.get(RuleId::from_raw(0)).unwrap().clone();
    second.id = RuleId::from_raw(1);
    second.label = RuleLabel::new("r/from_s_other");
    second.role = Some(RoleId::from_raw(1));
    p.rules.push(second).unwrap();
    for (i, n) in ["LA", "LB"].iter().enumerate() {
        p.lattices
            .push(LatticeDef {
                id: LatticeTypeId::from_raw(i as u32),
                name: name(n),
                ctor: LatticeCtor::Max(u),
                ops: vec![],
                height: blossom_value::class::HeightClass::Acc,
                laws: blossom_value::class::LawStatus::Builtin,
                distributive: true,
                dense_domain: None,
            })
            .unwrap();
    }
    for (i, ctor) in [GroupCtor::Z, GroupCtor::Zn(3)].into_iter().enumerate() {
        p.groups
            .push(GroupDef {
                id: GroupTypeId::from_raw(i as u32),
                ctor,
                ring: true,
            })
            .unwrap();
    }
    let props = FnProps {
        classes: vec![],
        injective: blossom_value::class::Claim::Absent,
        commutative: blossom_value::class::Claim::Absent,
        associative: blossom_value::class::Claim::Absent,
        idempotent: blossom_value::class::Claim::Absent,
        stable_after: None,
        metered: true,
    };
    for (i, n) in ["fa", "fb"].iter().enumerate() {
        p.fns
            .push(FnDecl {
                id: FnId::from_raw(i as u32),
                name: name(n),
                params: vec![],
                ret: u,
                vars: IndexVec::new(),
            body: FnBody::Builtin(BuiltinFn::Rand),
                props: props.clone(),
            })
            .unwrap();
    }
    for (i, n) in ["PA", "PB"].iter().enumerate() {
        p.params
            .push(ParamDecl {
                id: ParamId::from_raw(i as u32),
                name: name(n),
                ty: u,
                default: Some(ConstId::from_raw(i as u32)),
                span: span(),
            })
            .unwrap();
    }
    for i in 0..2 {
        p.udas
            .push(UdaDecl {
                id: UdaId::from_raw(i),
                state: u,
                init: FnId::from_raw(i),
                step: FnId::from_raw(i),
                combine: None,
                finish: FnId::from_raw(i),
                props: props.clone(),
            })
            .unwrap();
        p.services
            .push(ServiceDecl {
                id: ServiceId::from_raw(i),
                name: name(if i == 0 { "sa" } else { "sb" }),
                call: RelId::from_raw(0),
                result: RelId::from_raw(1),
            })
            .unwrap();
    }
    for (i, n) in ["ca", "cb"].iter().enumerate() {
        p.constructs
            .push(Construct {
                id: ConstructId::from_raw(i as u32),
                kind: ConstructKind::Outer,
                rules: vec![],
                rels: vec![],
                surface: SurfaceRef {
                    module: name("test"),
                    label: Some(s(n)),
                    stmt: None,
                    span: span(),
                },
            })
            .unwrap();
        let stable = format!("test::{n}#0");
        p.sites
            .push(Site {
                id: SiteId::from_raw(i as u32),
                key: RuleLabel::new(stable.clone()).hash,
                stable: stable.into(),
                kind: SiteKind::Choose,
                construct: ConstructId::from_raw(i as u32),
            })
            .unwrap();
        p.invariants
            .push(InvariantDecl {
                id: InvariantId::from_raw(i as u32),
                name: name(n),
                action: ViolationAction::Record,
                span: span(),
            })
            .unwrap();
    }
    for (_, rule) in p.rules.iter_enumerated_mut() {
        rule.body
            .vars
            .push(VarDecl {
                name: s("Unused"),
                ty: u,
                non_bottom: false,
            })
            .unwrap();
    }
    let first = ValidatedProgram::validate(p.clone()).unwrap().digest();
    struct Swap([bool; 15]);
    macro_rules! swap_ids {($($method:ident:$ty:ident:$index:expr),*)=>{$(fn $method(&mut self,id:$ty)->$ty{if self.0[$index]{$ty::from_raw(1-id.raw())}else{id}})*};}
    impl Mapper for Swap {
        swap_ids!(typeid:TypeId:0,latticetypeid:LatticeTypeId:1,grouptypeid:GroupTypeId:2,constid:ConstId:3,paramid:ParamId:4,fnid:FnId:5,udaid:UdaId:6,serviceid:ServiceId:7,roleid:RoleId:8,relid:RelId:9,ruleid:RuleId:10,siteid:SiteId:11,constructid:ConstructId:12,invariantid:InvariantId:13,varid:VarId:14);
    }

    let mut q = p.remap(&mut Swap(flags));
    if flags[0] {
        q.types = blossom_value::TypeTable::new();
        q.types.insert(TypeDef::Bool).unwrap();
        q.types.insert(TypeDef::Int(IntTy::U64)).unwrap();
    }
    if flags[3] {
        q.consts.as_mut_slice().swap(0, 1)
    }
    if flags[1] {
        q.lattices.as_mut_slice().swap(0, 1)
    }
    if flags[2] {
        q.groups.as_mut_slice().swap(0, 1)
    }
    if flags[4] {
        q.params.as_mut_slice().swap(0, 1)
    }
    if flags[5] {
        q.fns.as_mut_slice().swap(0, 1)
    }
    if flags[6] {
        q.udas.as_mut_slice().swap(0, 1)
    }
    if flags[7] {
        q.services.as_mut_slice().swap(0, 1)
    }
    if flags[8] {
        q.roles.as_mut_slice().swap(0, 1)
    }
    if flags[9] {
        q.rels.as_mut_slice().swap(0, 1)
    }
    if flags[10] {
        q.rules.as_mut_slice().swap(0, 1)
    }
    if flags[11] {
        q.sites.as_mut_slice().swap(0, 1)
    }
    if flags[12] {
        q.constructs.as_mut_slice().swap(0, 1)
    }
    if flags[13] {
        q.invariants.as_mut_slice().swap(0, 1)
    }
    if flags[14] {
        for (_, rule) in q.rules.iter_enumerated_mut() {
            rule.body.vars.as_mut_slice().swap(0, 1)
        }
    }
    assert_eq!(first, ValidatedProgram::validate(q).unwrap().digest());
}

#[test]
fn digest_invariant_under_every_id_space() {
    assert_digest_permutation([true; 15]);
}
proptest::proptest! {
    #[test]
    fn digest_invariant_under_renumbering(flags in proptest::array::uniform15(proptest::bool::ANY)) { assert_digest_permutation(flags); }
}

#[test]
fn project_role_keeps_channel_send_and_receive_sides() {
    let mut p = base();
    let u = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
    let node = p.types.insert(TypeDef::Node(None)).unwrap();
    let a = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(0),
            name: name("A"),
            kind: RoleKind::Process,
        })
        .unwrap();
    let b = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(1),
            name: name("B"),
            kind: RoleKind::Process,
        })
        .unwrap();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().role = Some(b);
    let mut c = rel(
        2,
        "msg",
        node,
        RelClass::Channel(ChannelDecl {
            form: ChannelForm::Direction { src: a, dst: b },
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
    );
    c.schema.cols.push(Column {
        name: s("value"),
        ty: u,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    });
    c.schema.key.push(ColIdx::from_raw(1));
    p.rels.push(c).unwrap();
    let dest = p
        .consts
        .push(blossom_value::Value::Node(blossom_value::NodeId(1)))
        .unwrap();
    let body = p.rules.get(RuleId::from_raw(0)).unwrap().body.clone();
    p.rules
        .push(Rule {
            id: RuleId::from_raw(1),
            label: RuleLabel::new("a/send"),
            kind: RuleKind::Async,
            head: Head {
                rel: RelId::from_raw(2),
                args: vec![
                    HeadArg::Term(Term::Const(dest)),
                    HeadArg::Term(Term::Var(VarId::from_raw(0))),
                ],
                mode: HeadMode::Insert,
            },
            body,
            role: Some(a),
            construct: None,
            span: span(),
        })
        .unwrap();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().body.lits = vec![Literal::Pos(Atom {
        rel: RelId::from_raw(2),
        args: vec![Term::Wild, Term::Var(VarId::from_raw(0))],
        sender: None,
        principal: None,
        weight: None,
        spec: None,
        span: span(),
    })];
    let valid = ValidatedProgram::validate(p).unwrap();
    let send = valid.project(a).unwrap();
    let receive = valid.project(b).unwrap();
    assert_eq!(send.get().rules.len(), 1);
    assert_eq!(receive.get().rules.len(), 1);
    assert_eq!(send.get().rules.get(RuleId::from_raw(0)).unwrap().kind, RuleKind::Async);
    assert_eq!(
        receive.get().rules.get(RuleId::from_raw(0)).unwrap().kind,
        RuleKind::Deductive
    );
    let printed = crate::printer::print(valid.get());
    assert!(printed.contains("msg(@Node(NodeId(1)), X)@async"));
    assert!(printed.contains("msg(X)"));
}
#[test]
fn project_role_keeps_endpoint_without_handler() {
    let mut p = Program::new(meta());
    let node = p.types.insert(TypeDef::Node(None)).unwrap();
    for n in ["A", "B", "C"] {
        p.roles
            .push(RoleDecl {
                id: RoleId::from_raw(p.roles.len() as u32),
                name: name(n),
                kind: RoleKind::Process,
            })
            .unwrap();
    }
    let mut channel = rel(
        0,
        "ingress",
        node,
        RelClass::Channel(ChannelDecl {
            form: ChannelForm::Direction {
                src: RoleId::from_raw(0),
                dst: RoleId::from_raw(1),
            },
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
    );
    channel.schema.cols[0].hidden_dest = true;
    p.rels.push(channel).unwrap();
    let valid = ValidatedProgram::validate(p).unwrap();
    for id in [0, 1] {
        let view = valid.project(RoleId::from_raw(id)).unwrap();
        assert_eq!(view.get().rels.len(), 1);
        assert_eq!(view.get().rules.len(), 0);
    }
    assert_eq!(valid.project(RoleId::from_raw(2)).unwrap().get().rels.len(), 0);
}
#[test]
fn project_role_trims_shared_construct_membership() {
    let mut p = base();
    for n in ["A", "B"] {
        p.roles
            .push(RoleDecl {
                id: RoleId::from_raw(p.roles.len() as u32),
                name: name(n),
                kind: RoleKind::Process,
            })
            .unwrap();
    }
    let mut second = p.rules.get(RuleId::from_raw(0)).unwrap().clone();
    second.id = RuleId::from_raw(1);
    second.label = RuleLabel::new("b/rule");
    second.role = Some(RoleId::from_raw(1));
    second.construct = Some(ConstructId::from_raw(0));
    p.rules.push(second).unwrap();
    let first = p.rules.get_mut(RuleId::from_raw(0)).unwrap();
    first.role = Some(RoleId::from_raw(0));
    first.construct = Some(ConstructId::from_raw(0));
    p.constructs
        .push(Construct {
            id: ConstructId::from_raw(0),
            kind: ConstructKind::Outer,
            rules: vec![RuleId::from_raw(0), RuleId::from_raw(1)],
            rels: vec![],
            surface: SurfaceRef {
                module: name("m"),
                label: None,
                stmt: None,
                span: span(),
            },
        })
        .unwrap();
    let valid = ValidatedProgram::validate(p).unwrap();
    for id in [0, 1] {
        let view = valid.project(RoleId::from_raw(id)).unwrap();
        assert_eq!(view.get().rules.len(), 1);
        assert_eq!(
            view.get().constructs.get(ConstructId::from_raw(0)).unwrap().rules.len(),
            1
        );
    }
}
#[test]
fn digest_invariant_under_equal_construct_sort_keys() {
    fn program(reverse: bool) -> Program {
        let mut p = base();
        let ty = p.types.lookup(&TypeDef::Int(IntTy::U64)).unwrap();
        let names = if reverse { ["m$b", "m$a"] } else { ["m$a", "m$b"] };
        for (i, n) in names.into_iter().enumerate() {
            let construct = ConstructId::from_raw(i as u32);
            let id = RelId::from_raw((i + 2) as u32);
            let mut decl = rel(id.raw(), n, ty, RelClass::Idb);
            decl.origin = Origin::Generated { construct };
            p.rels.push(decl).unwrap();
            p.constructs
                .push(Construct {
                    id: construct,
                    kind: ConstructKind::Outer,
                    rules: vec![],
                    rels: vec![id],
                    surface: SurfaceRef {
                        module: name("m"),
                        label: None,
                        stmt: None,
                        span: span(),
                    },
                })
                .unwrap();
        }
        p
    }
    let a = ValidatedProgram::validate(program(false)).unwrap();
    let b = ValidatedProgram::validate(program(true)).unwrap();
    assert_eq!(a.digest(), b.digest());
}

#[test]
fn validator_v6_reserves_violation_relation() {
    let mut p = good();
    p.rels.get_mut(RelId::from_raw(0)).unwrap().name = name("violation");
    assert!(has(p, 6));
    let mut b = IrBuilder::new(meta(), FrontendKind::Blossom);
    let ty = b.types().insert(TypeDef::Int(IntTy::U64)).unwrap();
    assert!(b.declare_relation(rel(0, "violation", ty, RelClass::Idb)).is_err());
}
#[test]
fn validator_v2_violation_head_requires_generated_relation() {
    let mut p = good();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().head.mode = HeadMode::Violation {
        invariant: InvariantId::from_raw(0),
    };
    p.invariants
        .push(InvariantDecl {
            id: InvariantId::from_raw(0),
            name: name("x"),
            action: ViolationAction::Record,
            span: span(),
        })
        .unwrap();
    assert!(has(p, 2));
}

#[test]
fn project_role_prunes_unreferenced_shared_items() {
    let mut p = base();
    let a = p
        .roles
        .push(RoleDecl {
            id: RoleId::from_raw(0),
            name: name("A"),
            kind: RoleKind::Process,
        })
        .unwrap();
    p.rules.get_mut(RuleId::from_raw(0)).unwrap().role = Some(a);
    let ty = p.types.insert(TypeDef::Bool).unwrap();
    p.rels.push(rel(2, "unused", ty, RelClass::Idb)).unwrap();
    let props = FnProps {
        classes: vec![],
        injective: blossom_value::class::Claim::Absent,
        commutative: blossom_value::class::Claim::Absent,
        associative: blossom_value::class::Claim::Absent,
        idempotent: blossom_value::class::Claim::Absent,
        stable_after: None,
        metered: true,
    };
    p.fns
        .push(FnDecl {
            id: FnId::from_raw(0),
            name: name("unused_fn"),
            params: vec![],
            ret: ty,
            vars: IndexVec::new(),
            body: FnBody::Builtin(BuiltinFn::ZWeight {
                rel: RelId::from_raw(2),
            }),
            props,
        })
        .unwrap();
    let projected = ValidatedProgram::validate(p).unwrap().project(a).unwrap();
    assert_eq!(projected.get().rels.len(), 2);
    assert_eq!(projected.get().fns.len(), 0);
    assert_eq!(projected.get().types.len(), 1);
}

#[test]
fn plan_digest_changes_with_profile() {
    use crate::plan::*;
    let digest = ValidatedProgram::validate(base()).unwrap().digest();
    let plan = PhysicalProgram {
        program: digest,
        role: None,
        planner_version: 1,
        abi: 1,
        profile: PlanProfile::Production,
        features: vec![],
        rels: IndexVec::new(),
        strata: IndexVec::new(),
        temporal: TemporalPlan {
            inductive: vec![],
            asynchronous: vec![],
        },
        natives: IndexVec::new(),
        agg_tables: IndexVec::new(),
        buffers: IndexVec::new(),
        ingest: IngestPlan {
            deliveries: vec![],
            timers: vec![],
            host_inputs: vec![],
            boot: None,
            recovered: None,
            table_fns: vec![],
            branching: vec![],
        },
        prov: ProvPlan {
            tier: ProvTier::Off,
            rules: vec![],
            annotation_cols: vec![],
        },
        digests: DigestPlan {
            state: false,
            outbox: false,
            choices: false,
            changed: vec![],
        },
        empty_tick_effects: false,
        limits: PlanLimits {
            iterations: 1,
            batch_size: 1,
            fuel: 1,
            max_group: 1,
            natives: false,
            elastic_threshold: None,
        },
    };
    let a = plan.digest().unwrap();
    let mut b = plan.clone();
    b.profile = PlanProfile::Literal;
    assert_ne!(a, b.digest().unwrap());
    assert_eq!(a, plan.digest().unwrap());
}
