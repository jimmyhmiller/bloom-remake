use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{QualName, Symbol, TypeId};
use blossom_ir::build::{FrontendKind, IrBuilder};
use blossom_ir::core::{Column, HeightClass, LatticeCtor, LatticeDef, LawStatus, Program, ProgramMeta};
use blossom_value::time::NodeId;
use blossom_value::types::{EnumDef, FieldDef, FieldNo, IntTy, StructDef, VariantDef};
use blossom_value::value::{IntValue, LatValue};
use blossom_value::{TypeDef, Value};
use proptest::prelude::*;

use crate::codec::{Codec, NodeEncoding, WireError, WireLimits};
use crate::frame::{Batch, ChannelSchema, Frame, Hello, Peer, RejectReason};

struct Fixture {
    program: Program,
    scalars: Vec<TypeId>,
    lmax: TypeId,
    lset: TypeId,
    lmap: TypeId,
    product: TypeId,
    node: TypeId,
}

fn field(name: &str, ty: TypeId) -> FieldDef {
    FieldDef {
        name: Symbol::intern(name),
        ty,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        renamed_from: None,
    }
}

fn fixture() -> Fixture {
    let mut b = IrBuilder::new(
        ProgramMeta {
            name: Symbol::intern("wire"),
            version: 1,
            edition: 1,
            compiler: "test".into(),
            prf_version: 1,
            encoding_version: 1,
            program_id: [0; 16],
        },
        FrontendKind::Blossom,
    );
    let mut t = |d: TypeDef| b.types().insert(d).unwrap();
    let u64t = t(TypeDef::Int(IntTy::U64));
    let i64t = t(TypeDef::Int(IntTy::I64));
    let str_t = t(TypeDef::Str);
    let mut scalars = vec![
        t(TypeDef::Bool),
        t(TypeDef::Int(IntTy::U8)),
        u64t,
        t(TypeDef::Int(IntTy::I8)),
        i64t,
        t(TypeDef::Int(IntTy::U128)),
        t(TypeDef::Int(IntTy::I128)),
        t(TypeDef::F64),
        str_t,
        t(TypeDef::Bytes),
        t(TypeDef::Unit),
        t(TypeDef::Duration),
        t(TypeDef::Instant),
        t(TypeDef::Session),
        t(TypeDef::Principal),
    ];
    let node = t(TypeDef::Node(None));
    let tuple = t(TypeDef::Tuple(vec![u64t, str_t]));
    let opt = t(TypeDef::Option(u64t));
    let vec_t = t(TypeDef::Vec(i64t));
    let set_t = t(TypeDef::Set(str_t));
    let map_t = t(TypeDef::Map(str_t, u64t));
    let st = t(TypeDef::Struct(StructDef {
        name: QualName::parse_dotted("S").unwrap(),
        fields: vec![field("a", u64t), field("b", opt)],
        reserved: Vec::new(),
    }));
    let en = t(TypeDef::Enum(EnumDef {
        name: QualName::parse_dotted("E").unwrap(),
        variants: vec![
            VariantDef {
                name: Symbol::intern("A"),
                number: 0,
                payload: Vec::new(),
                since: None,
            },
            VariantDef {
                name: Symbol::intern("B"),
                number: 3,
                payload: vec![field("x", str_t)],
                since: None,
            },
            VariantDef {
                name: Symbol::intern("Unknown"),
                number: 9,
                payload: Vec::new(),
                since: None,
            },
        ],
        unknown: Some(9),
        reserved: Vec::new(),
    }));
    scalars.extend([tuple, opt, vec_t, set_t, map_t, st, en]);
    let lattice = |ctor: LatticeCtor, name: &str, b: &mut IrBuilder| {
        let id = b
            .declare_lattice(LatticeDef {
                id: blossom_base::LatticeTypeId::from_raw(0),
                name: QualName::parse_dotted(name).unwrap(),
                ctor,
                ops: Vec::new(),
                height: HeightClass::Unknown,
                laws: LawStatus::Builtin,
                distributive: true,
                dense_domain: None,
            })
            .unwrap();
        b.types().insert(TypeDef::Lattice(id)).unwrap()
    };
    let lmax = lattice(LatticeCtor::Max(u64t), "LMax", &mut b);
    let lset = lattice(LatticeCtor::Set(str_t), "LSet", &mut b);
    let lmax_id = blossom_base::LatticeTypeId::from_raw(0);
    let lmap = lattice(LatticeCtor::Map(str_t, lmax_id), "LMap", &mut b);
    let lset_id = blossom_base::LatticeTypeId::from_raw(1);
    let lmap_id = blossom_base::LatticeTypeId::from_raw(2);
    let product = lattice(
        LatticeCtor::Product {
            name: QualName::parse_dotted("Cart").unwrap(),
            fields: vec![
                (Symbol::intern("hi"), lmax_id),
                (Symbol::intern("tags"), lset_id),
                (Symbol::intern("seen"), lmap_id),
            ],
        },
        "Cart",
        &mut b,
    );
    Fixture {
        program: b.program().clone(),
        scalars,
        lmax,
        lset,
        lmap,
        product,
        node,
    }
}

fn roundtrip(c: &Codec<'_>, ty: TypeId, v: &Value) -> Value {
    let mut buf = Vec::new();
    c.encode_value(ty, v, &mut buf).unwrap();
    let mut input = buf.as_slice();
    let back = c.decode_value(ty, &mut input).unwrap();
    assert!(input.is_empty(), "trailing bytes for {v:?}");
    back
}

fn values_of_every_type() -> impl Strategy<Value = (usize, Value)> {
    let f = fixture();
    let strategies: Vec<BoxedStrategy<(usize, Value)>> = f
        .scalars
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            blossom_value::arbitrary::value_for(&f.program.types, *ty)
                .unwrap()
                .prop_map(move |v| (i, v))
                .boxed()
        })
        .collect();
    proptest::strategy::Union::new(strategies)
}

proptest! {
    #[test]
    fn every_value_round_trips((i, v) in values_of_every_type()) {
        let f = fixture();
        let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
        let ty = f.scalars[i];
        // F64 compares by bit pattern through Value's canonical order.
        prop_assert_eq!(roundtrip(&c, ty, &v), v);
    }

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..64), i in 0usize..22) {
        let f = fixture();
        let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
        let ty = f.scalars[i % f.scalars.len()];
        let mut input = bytes.as_slice();
        let _ = c.decode_value(ty, &mut input);
        let _ = Frame::decode(bytes.first().copied().unwrap_or(0), bytes.get(1..).unwrap_or(&[]), &WireLimits::default());
    }
}

#[test]
fn lattices_round_trip() {
    let f = fixture();
    let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
    let u = |n: u64| Value::Int(IntValue::U64(n));
    let e = |n: u64| LatValue::Elem(Arc::new(u(n)));
    for v in [LatValue::Bottom, e(7)] {
        assert_eq!(roundtrip(&c, f.lmax, &Value::Lattice(v.clone())), Value::Lattice(v));
    }
    let set = LatValue::Set(Arc::new(BTreeSet::from([Value::str("a"), Value::str("b")])));
    assert_eq!(
        roundtrip(&c, f.lset, &Value::Lattice(set.clone())),
        Value::Lattice(set.clone())
    );
    let map = LatValue::Map(Arc::new(BTreeMap::from([
        (Value::str("k"), e(3)),
        (Value::str("z"), e(9)),
    ])));
    assert_eq!(
        roundtrip(&c, f.lmap, &Value::Lattice(map.clone())),
        Value::Lattice(map.clone())
    );
    // A product: its fields in order, ⊥ ones included.
    for v in [
        LatValue::Seq(Arc::from(vec![e(2), set.clone(), map.clone()])),
        LatValue::Seq(Arc::from(vec![
            LatValue::Bottom,
            LatValue::Set(Arc::new(BTreeSet::new())),
            map,
        ])),
    ] {
        assert_eq!(roundtrip(&c, f.product, &Value::Lattice(v.clone())), Value::Lattice(v));
    }
    // A product value with the wrong number of fields is refused both ways.
    let short = Value::Lattice(LatValue::Seq(Arc::from(vec![e(2)])));
    assert!(c.encode_value(f.product, &short, &mut Vec::new()).is_err());
}

/// A client member (docs/design/CLIENTS.md §2) is written with its admitting node's name: it keeps its identity when
/// the deployment's nodes are renumbered.
#[test]
fn client_members_by_name_survive_renumbering() {
    let f = fixture();
    let before = Codec::new(
        &f.program,
        NodeEncoding::ByName(Arc::from(vec![Arc::from("a"), Arc::from("b")])),
        WireLimits::default(),
    );
    let member = NodeId::client(NodeId(1), 42).unwrap();
    let mut buf = Vec::new();
    before.encode_value(f.node, &Value::Node(member), &mut buf).unwrap();
    let after = Codec::new(
        &f.program,
        NodeEncoding::ByName(Arc::from(vec![Arc::from("b"), Arc::from("a")])),
        WireLimits::default(),
    );
    assert_eq!(
        after.decode_value(f.node, &mut buf.as_slice()).unwrap(),
        Value::Node(NodeId::client(NodeId(0), 42).unwrap())
    );
}

#[test]
fn nodes_by_name_survive_renumbering() {
    let f = fixture();
    let before = Codec::new(
        &f.program,
        NodeEncoding::ByName(Arc::from(vec![Arc::from("a"), Arc::from("b")])),
        WireLimits::default(),
    );
    let mut buf = Vec::new();
    before.encode_value(f.node, &Value::Node(NodeId(1)), &mut buf).unwrap();
    let after = Codec::new(
        &f.program,
        NodeEncoding::ByName(Arc::from(vec![Arc::from("b"), Arc::from("c"), Arc::from("a")])),
        WireLimits::default(),
    );
    assert_eq!(
        after.decode_value(f.node, &mut buf.as_slice()).unwrap(),
        Value::Node(NodeId(0))
    );
    let gone = Codec::new(
        &f.program,
        NodeEncoding::ByName(Arc::from(vec![Arc::from("a")])),
        WireLimits::default(),
    );
    assert!(matches!(
        gone.decode_value(f.node, &mut buf.as_slice()),
        Err(WireError::Malformed(_))
    ));
}

#[test]
fn rows_use_field_numbers_and_skip_unknown_fields() {
    let f = fixture();
    let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
    let (u64t, str_t) = (f.scalars[2], f.scalars[8]);
    let col = |name: &str, ty, n: Option<u32>| Column {
        name: Symbol::intern(name),
        ty,
        field_no: n.map(FieldNo),
        default: None,
        since: None,
        deprecated: None,
        hidden_dest: false,
    };
    // A newer sender has an extra field #7; the receiver's columns are #2 and #5, declared out of order.
    let newer = [
        col("k", str_t, Some(5)),
        col("id", u64t, Some(2)),
        col("extra", u64t, Some(7)),
    ];
    let older = [col("k", str_t, Some(5)), col("id", u64t, Some(2))];
    let row = [
        Value::str("key"),
        Value::Int(IntValue::U64(4)),
        Value::Int(IntValue::U64(99)),
    ];
    let mut buf = Vec::new();
    c.encode_row(&newer, &row, &mut buf).unwrap();
    let got = c.decode_row(&older, &mut buf.as_slice()).unwrap();
    assert_eq!(got, vec![Value::str("key"), Value::Int(IntValue::U64(4))]);
    // A missing field is an error, never a default this build does not have.
    let wider = [
        col("k", str_t, Some(5)),
        col("id", u64t, Some(2)),
        col("new", u64t, Some(8)),
    ];
    assert!(c.decode_row(&wider, &mut buf.as_slice()).is_err());
}

#[test]
fn unknown_enum_variants_keep_their_bytes() {
    let f = fixture();
    let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
    let en = f.scalars[21];
    // Variant #12 does not exist here: it decodes to #[unknown] (#9) and re-encodes unchanged.
    let mut wire = Vec::new();
    crate::codec::put_varint(&mut wire, 12);
    crate::codec::put_varint(&mut wire, 3);
    wire.extend_from_slice(&[1, 2, 3]);
    let v = c.decode_value(en, &mut wire.as_slice()).unwrap();
    assert!(matches!(
        v,
        Value::UnknownVariant {
            variant: 9,
            wire_number: 12,
            ..
        }
    ));
    let mut again = Vec::new();
    c.encode_value(en, &v, &mut again).unwrap();
    assert_eq!(again, wire);
}

#[test]
fn limits_are_enforced() {
    let f = fixture();
    let c = Codec::new(&f.program, NodeEncoding::Dense, WireLimits::default());
    let vec_t = f.scalars[17];
    // A vector claiming 2^40 elements in a 3-byte payload.
    let mut inner = Vec::new();
    crate::codec::put_varint(&mut inner, 1 << 40);
    let mut wire = Vec::new();
    crate::codec::put_varint(&mut wire, inner.len() as u64);
    wire.extend_from_slice(&inner);
    assert!(matches!(
        c.decode_value(vec_t, &mut wire.as_slice()),
        Err(WireError::Limit(_))
    ));
}

#[test]
fn frames_round_trip() {
    let limits = WireLimits::default();
    let frames = vec![
        Frame::Hello(Hello {
            proto: 1,
            deployment: [1; 16],
            program_id: [2; 16],
            program_version: 3,
            peer: Peer::Client {
                principal: "alice".into(),
            },
            directory: [4; 16],
            restarts: 5,
            boot_nonce: 6,
            channels: vec![ChannelSchema {
                sid: 1,
                name: "put".into(),
                hash: [7; 16],
            }],
        }),
        Frame::HelloOk {
            accepted_version: 3,
            sids: vec![1, 2],
        },
        Frame::Reject {
            reason: RejectReason::SchemaMismatch,
            detail: "put".into(),
        },
        Frame::GoAway { reason: 1 },
        Frame::Batch(Batch {
            sid: 2,
            send_tick: 44,
            kind: 0,
            count: 1,
            body: vec![1, 8, 3],
        }),
        // A client member's link (docs/design/CLIENTS.md §3): new, then resuming.
        Frame::Hello(Hello {
            proto: 1,
            deployment: [1; 16],
            program_id: [2; 16],
            program_version: 3,
            peer: Peer::Member {
                role: "Browser".into(),
                part: [5; 16],
                token: None,
                received: 0,
                acked: 0,
            },
            directory: [4; 16],
            restarts: 0,
            boot_nonce: 0,
            channels: Vec::new(),
        }),
        Frame::Hello(Hello {
            proto: 1,
            deployment: [1; 16],
            program_id: [2; 16],
            program_version: 3,
            peer: Peer::Member {
                role: "Browser".into(),
                part: [6; 16],
                token: Some(vec![9; 20]),
                received: 17,
                acked: 4,
            },
            directory: [4; 16],
            restarts: 0,
            boot_nonce: 0,
            channels: Vec::new(),
        }),
        Frame::Welcome {
            member: 0x8000_0003,
            token: vec![9; 20],
            resumed: true,
            floor: 4,
            seed: [5; 16],
        },
        Frame::Msg {
            seq: 300,
            batch: Batch {
                sid: 1,
                send_tick: 9,
                kind: 0,
                count: 1,
                body: vec![1, 8, 3],
            },
        },
        Frame::Ack { seq: 300 },
    ];
    for f in frames {
        let bytes = f.encode();
        let back = Frame::read(&mut bytes.as_slice(), &limits).unwrap().unwrap();
        assert_eq!(back, f);
    }
}
