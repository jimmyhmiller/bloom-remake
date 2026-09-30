//! Slice 6: pure functions (LANGUAGE §16.1) and the built-in library (Appendix B), end to end. The fixture
//! `fixtures/functions/library.bls` applies every library function and combinator inside functions; random inputs
//! run on the oracle and on the engine, which must agree at every tick, and every view must equal what this file
//! computes for it directly in Rust (a third, independent implementation).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::EngineEvaluator;
use blossom_sim::FaultSchedule;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::sync::SyncRun;
use blossom_value::Value;
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;

/// SplitMix64.
#[cfg(test)]
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

#[cfg(test)]
fn compile(name: &str) -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/functions")
        .join(name);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().unwrap(), &nodes);
    result.unwrap_or_else(|e| panic!("{name}: {e:?}")).0
}

/// Runs `artifact` on the oracle and on the engine; they must agree at every tick. Returns the oracle's run.
#[cfg(test)]
fn differential(artifact: &BlsArtifact, inputs: &[InputEvent], last: u64) -> SyncRun {
    let sim = BlsSim::new(artifact, blossom_value::Seed::from_u64(0)).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap_or_else(|e| panic!("the oracle failed: {e}"));
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, inputs, Tick(last), round, &FaultSchedule::default(), false)
        .unwrap_or_else(|e| panic!("the engine failed: {e}"));
    assert_eq!(reference.rounds.len(), mine.rounds.len());
    for (t, (ra, rb)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        for (x, y) in ra.iter().zip(rb) {
            assert_eq!(x.instance, y.instance, "tick {t}: the oracle and the engine differ");
        }
    }
    reference
}

#[cfg(test)]
fn u(n: u64) -> Value {
    Value::Int(IntValue::U64(n))
}

#[cfg(test)]
fn vec_u(xs: impl IntoIterator<Item = u64>) -> Value {
    Value::Vec(xs.into_iter().map(u).collect())
}

#[cfg(test)]
fn tuple(xs: Vec<Value>) -> Value {
    Value::Tuple(xs.into())
}

#[cfg(test)]
fn opt(x: Option<Value>) -> Value {
    Value::Option(x.map(Arc::new))
}

#[cfg(test)]
fn bytes(b: &[u8]) -> Value {
    Value::Bytes(Arc::from(b))
}

/// The value each view's function computes, written directly.
#[cfg(test)]
fn expected(view: &str, n: u64, s: &str, b: &[u8]) -> (Value, Value) {
    let evens: Vec<u64> = (0..n).filter(|i| i % 2 == 0).collect();
    match view {
        "v_sum" => (u(n), u((0..n).sum())),
        "v_evens" => (u(n), vec_u(evens)),
        "v_scaled" => (u(n), vec_u((0..n).map(|i| i * n))),
        "v_thirds" => (u(n), vec_u((0..n).filter(|i| i % 3 == 0).map(|i| i / 3))),
        "v_triangle" => (u(n), vec_u((0..n).map(|i| i + (0..i).sum::<u64>()))),
        "v_quant" => (
            u(n),
            tuple(vec![Value::Bool(n <= 5), Value::Bool(n >= 8), Value::Bool(false)]),
        ),
        "v_ends" => (
            u(n),
            tuple(vec![
                opt(evens.first().map(|x| u(*x))),
                opt(evens.last().map(|x| u(*x))),
                opt(evens.get(2).map(|x| u(*x))),
            ]),
        ),
        "v_reshaped" => {
            let mut v = evens;
            v.push(n);
            v.extend([0, 10]);
            v.reverse();
            (u(n), vec_u(v))
        }
        "v_shadowed" => (u(n), u(3 * n + 2)),
        "v_words" => (
            Value::Str(s.into()),
            Value::Vec(
                s.split_whitespace()
                    .enumerate()
                    .map(|(i, w)| tuple(vec![u(i as u64), Value::Str(w.to_lowercase().into())]))
                    .collect(),
            ),
        ),
        "v_blank" => (Value::Str(s.into()), Value::Bool(s.split_whitespace().next().is_none())),
        "v_fields" => (
            bytes(b),
            opt((b.len() >= 3).then(|| tuple(vec![bytes(&b[0..1]), bytes(&b[1..3])]))),
        ),
        "v_doubled" => (bytes(b), bytes(&[b, b].concat())),
        "v_defaults" => {
            let o = [0u64, 3, 6, 9].get(n as usize).copied();
            (
                u(n),
                tuple(vec![
                    u(o.unwrap_or(100)),
                    Value::Bool(o.is_some()),
                    Value::Bool(o.is_none()),
                ]),
            )
        }
        "v_classify" => (
            u(n),
            Value::Str(
                match (n / 10, n % 10) {
                    (0, _) => "small",
                    (_, 0) => "round",
                    _ => "big",
                }
                .into(),
            ),
        ),
        "v_range" => (u(n), u(n + 1)),
        other => panic!("no expectation for {other}"),
    }
}

#[cfg(test)]
const VIEWS: &[&str] = &[
    "v_sum",
    "v_evens",
    "v_scaled",
    "v_thirds",
    "v_triangle",
    "v_quant",
    "v_ends",
    "v_reshaped",
    "v_shadowed",
    "v_words",
    "v_blank",
    "v_fields",
    "v_doubled",
    "v_defaults",
    "v_classify",
    "v_range",
];

#[test]
fn every_library_function_agrees_on_both_evaluators_and_with_its_definition() {
    let artifact = compile("library.bls");
    let e = artifact.rel_named("e").unwrap();
    let words = ["Foo", "bar", "BAZ", "qUx", "ümlaut", "ΣΙΣΥΦΟΣ"];
    let spaces = [" ", "  ", "\t", "\n ", ""];
    let mut checked = 0;
    for seed in 0..8u64 {
        let mut rng = Rng(seed);
        let last = 12u64;
        let mut per_tick: Vec<Vec<(u64, String, Vec<u8>)>> = vec![Vec::new(); last as usize + 1];
        let mut inputs = Vec::new();
        for t in 1..last {
            for _ in 0..rng.below(4) {
                let n = rng.below(25);
                let mut s = String::new();
                for _ in 0..rng.below(4) {
                    s.push_str(spaces[rng.below(spaces.len() as u64) as usize]);
                    s.push_str(words[rng.below(words.len() as u64) as usize]);
                }
                s.push_str(spaces[rng.below(spaces.len() as u64) as usize]);
                let b: Vec<u8> = (0..rng.below(6)).map(|_| rng.below(256) as u8).collect();
                inputs.push(InputEvent {
                    node: NodeId(0),
                    tick: Tick(t),
                    rel: e,
                    row: Arc::from(vec![u(n), Value::Str(s.as_str().into()), bytes(&b)]),
                });
                per_tick[t as usize].push((n, s, b));
            }
        }
        let run = differential(&artifact, &inputs, last);
        for (t, rows) in per_tick.iter().enumerate() {
            let instance = &run.node_tick(Tick(t as u64), NodeId(0)).unwrap().instance;
            for view in VIEWS {
                let rel = artifact.rel_named(view).unwrap();
                let got: BTreeSet<Vec<Value>> = instance.rows(rel).map(|r| r.to_vec()).collect();
                let want: BTreeSet<Vec<Value>> = rows
                    .iter()
                    .map(|(n, s, b)| {
                        let (k, v) = expected(view, *n, s, b);
                        vec![k, v]
                    })
                    .collect();
                assert_eq!(got, want, "seed {seed} tick {t} view {view}");
                checked += want.len();
            }
        }
    }
    assert!(checked > 500, "only {checked} rows checked");
}

// ---------------------------------------------------------------- byte primitives (FOREIGN-PROTOCOLS §3)

/// A reference LEB128 decoder, written apart from both evaluators: accumulate in a u128, then check the range.
#[cfg(test)]
fn ref_uvarint(b: &[u8], p: usize) -> Option<(u64, usize)> {
    let mut v: u128 = 0;
    for (i, byte) in b.get(p..)?.iter().enumerate().take(10) {
        v |= u128::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return u64::try_from(v).ok().map(|v| (v, p + i + 1));
        }
    }
    None
}

#[cfg(test)]
fn ref_uvarint_bytes(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let more = n >= 0x80;
        out.push((n & 0x7f) as u8 | if more { 0x80 } else { 0 });
        n >>= 7;
        if !more {
            return out;
        }
    }
}

#[cfg(test)]
fn zigzag(k: i64) -> u64 {
    ((k as i128 * 2) ^ if k < 0 { -1 } else { 0 }) as u64
}

#[cfg(test)]
fn unzigzag(n: u64) -> i64 {
    if n.is_multiple_of(2) {
        (n / 2) as i64
    } else {
        -((n / 2) as i64) - 1
    }
}

/// One `e` row: the bytes, a position, and one integer of each type.
#[cfg(test)]
#[derive(Clone)]
struct Row {
    b: Vec<u8>,
    p: u64,
    a: u8,
    c: i8,
    d: u16,
    g: i16,
    h: u32,
    i: i32,
    j: u64,
    k: i64,
}

#[cfg(test)]
fn iv<T>(f: fn(T) -> IntValue, x: T) -> Value {
    Value::Int(f(x))
}

#[cfg(test)]
fn read_ref(b: &[u8], p: u64, n: usize) -> Option<[u8; 8]> {
    let p = usize::try_from(p).ok()?;
    let s = b.get(p..p.checked_add(n)?)?;
    let mut w = [0u8; 8];
    w[8 - n..].copy_from_slice(s);
    Some(w)
}

#[cfg(test)]
fn put_ref(b: &[u8], p: u64, enc: &[u8]) -> Value {
    let Some(p) = usize::try_from(p)
        .ok()
        .filter(|p| p.checked_add(enc.len()).is_some_and(|e| e <= b.len()))
    else {
        return opt(None);
    };
    let mut out = b.to_vec();
    out[p..p + enc.len()].copy_from_slice(enc);
    opt(Some(bytes(&out)))
}

#[cfg(test)]
fn encoded_ref(r: &Row) -> Vec<u8> {
    let mut x = vec![r.a];
    x.extend(r.c.to_be_bytes());
    x.extend(r.d.to_be_bytes());
    x.extend(r.g.to_be_bytes());
    x.extend(r.h.to_be_bytes());
    x.extend(r.i.to_be_bytes());
    x.extend(r.j.to_be_bytes());
    x.extend(r.k.to_be_bytes());
    x
}

/// The rows each view of `bytes.bls` holds for one `e` row.
#[cfg(test)]
fn bytes_expected(view: &str, r: &Row) -> Vec<Value> {
    let b = &r.b;
    let rd = |n: usize| read_ref(b, r.p, n);
    let varint_pair = |x: Option<(Value, usize)>| opt(x.map(|(v, next)| tuple(vec![v, u(next as u64)])));
    match view {
        "v_reads" => vec![
            bytes(b),
            u(r.p),
            tuple(vec![
                opt(rd(1).map(|w| iv(IntValue::U8, w[7]))),
                opt(rd(1).map(|w| iv(IntValue::I8, w[7] as i8))),
                opt(rd(2).map(|w| iv(IntValue::U16, u64::from_be_bytes(w) as u16))),
                opt(rd(2).map(|w| iv(IntValue::I16, u64::from_be_bytes(w) as u16 as i16))),
                opt(rd(4).map(|w| iv(IntValue::U32, u64::from_be_bytes(w) as u32))),
                opt(rd(4).map(|w| iv(IntValue::I32, u64::from_be_bytes(w) as u32 as i32))),
                opt(rd(8).map(|w| iv(IntValue::U64, u64::from_be_bytes(w)))),
                opt(rd(8).map(|w| iv(IntValue::I64, i64::from_be_bytes(w)))),
            ]),
        ],
        "v_encoded" => vec![bytes(&encoded_ref(r))],
        "v_decoded" => vec![opt(Some(tuple(vec![
            iv(IntValue::U8, r.a),
            iv(IntValue::I8, r.c),
            iv(IntValue::U16, r.d),
            iv(IntValue::I16, r.g),
            iv(IntValue::U32, r.h),
            iv(IntValue::I32, r.i),
            iv(IntValue::U64, r.j),
            iv(IntValue::I64, r.k),
        ])))],
        "v_puts" => vec![
            bytes(b),
            u(r.p),
            tuple(vec![
                put_ref(b, r.p, &[r.a]),
                put_ref(b, r.p, &r.g.to_be_bytes()),
                put_ref(b, r.p, &r.h.to_be_bytes()),
                put_ref(b, r.p, &r.k.to_be_bytes()),
            ]),
        ],
        "v_varints" => {
            let at = usize::try_from(r.p).ok().and_then(|p| ref_uvarint(b, p));
            vec![
                bytes(b),
                u(r.p),
                tuple(vec![
                    varint_pair(at.map(|(v, n)| (u(v), n))),
                    varint_pair(at.map(|(v, n)| (iv(IntValue::I64, unzigzag(v)), n))),
                ]),
            ]
        }
        "v_uvarint" => vec![u(r.j), bytes(&ref_uvarint_bytes(r.j))],
        "v_varint" => vec![iv(IntValue::I64, r.k), bytes(&ref_uvarint_bytes(zigzag(r.k)))],
        "v_trip" => {
            let len = ref_uvarint_bytes(r.j).len() + ref_uvarint_bytes(zigzag(r.k)).len();
            vec![
                u(r.j),
                iv(IntValue::I64, r.k),
                opt(Some(tuple(vec![u(r.j), iv(IntValue::I64, r.k), u(len as u64)]))),
            ]
        }
        "v_utf8" => vec![bytes(b), opt(std::str::from_utf8(b).ok().map(|s| Value::Str(s.into())))],
        other => panic!("no expectation for {other}"),
    }
}

#[cfg(test)]
const BYTE_VIEWS: &[&str] = &[
    "v_reads",
    "v_encoded",
    "v_decoded",
    "v_puts",
    "v_varints",
    "v_uvarint",
    "v_varint",
    "v_trip",
    "v_utf8",
];

/// Varints whose verdicts are fixed by the encoding: truncated, overlong, overflowing, non-minimal and extreme.
/// A varint's bytes and its decoding at position 0 (value, next position), if any.
#[cfg(test)]
type Golden = (Vec<u8>, Option<(u64, usize)>);

#[cfg(test)]
fn golden_varints() -> Vec<Golden> {
    let mut max = vec![0xff; 9];
    max.push(0x01);
    let mut over = vec![0xff; 9];
    over.push(0x02);
    let mut eleven = vec![0x80; 10];
    eleven.push(0x00);
    let mut ten_zero = vec![0x80; 9];
    ten_zero.push(0x00);
    vec![
        (vec![], None),
        (vec![0x80], None),
        (vec![0xff, 0xff], None),
        (vec![0x00], Some((0, 1))),
        (vec![0x7f], Some((127, 1))),
        (vec![0x80, 0x01], Some((128, 2))),
        (vec![0x80, 0x00], Some((0, 2))),
        (max, Some((u64::MAX, 10))),
        (over, None),
        (eleven, None),
        (ten_zero, Some((0, 10))),
    ]
}

#[test]
fn byte_primitives_round_trip_and_agree_on_both_evaluators() {
    let artifact = compile("bytes.bls");
    let e = artifact.rel_named("e").unwrap();
    let s = artifact.rel_named("s").unwrap();
    // The golden vectors pin the decoder apart from any implementation.
    for (b, want) in golden_varints() {
        assert_eq!(ref_uvarint(&b, 0), want, "the reference decoder on {b:02x?}");
    }
    let texts = ["", "kafka", "ünïcödé", "日本語", "a\u{0}b", "🙂 emoji"];
    let mut checked = 0;
    for seed in 0..10u64 {
        let mut rng = Rng(seed);
        let last = 10u64;
        let mut per_tick: Vec<Vec<Row>> = vec![Vec::new(); last as usize + 1];
        let mut inputs = Vec::new();
        for t in 1..last {
            let mut rows = Vec::new();
            if t == 1 {
                for (b, _) in golden_varints() {
                    rows.push((b, 0u64));
                }
            }
            for _ in 0..rng.below(6) {
                let n = rng.below(13);
                // Continuation bytes half the time, so varints run long, truncate and overflow.
                let b: Vec<u8> = (0..n)
                    .map(|_| {
                        if rng.below(2) == 0 {
                            0x80 | rng.below(128) as u8
                        } else {
                            rng.below(256) as u8
                        }
                    })
                    .collect();
                let p = rng.below(15);
                rows.push((b, p));
            }
            for (b, p) in rows {
                let x = rng.next();
                let r = Row {
                    b,
                    p,
                    a: x as u8,
                    c: (x >> 8) as i8,
                    d: (x >> 16) as u16,
                    g: (x >> 24) as i16,
                    h: rng.next() as u32,
                    i: rng.next() as i32,
                    j: if rng.below(3) == 0 { rng.below(300) } else { rng.next() },
                    k: if rng.below(3) == 0 {
                        rng.below(300) as i64 - 150
                    } else {
                        rng.next() as i64
                    },
                };
                inputs.push(InputEvent {
                    node: NodeId(0),
                    tick: Tick(t),
                    rel: e,
                    row: Arc::from(vec![
                        bytes(&r.b),
                        u(r.p),
                        iv(IntValue::U8, r.a),
                        iv(IntValue::I8, r.c),
                        iv(IntValue::U16, r.d),
                        iv(IntValue::I16, r.g),
                        iv(IntValue::U32, r.h),
                        iv(IntValue::I32, r.i),
                        u(r.j),
                        iv(IntValue::I64, r.k),
                    ]),
                });
                per_tick[t as usize].push(r);
            }
            inputs.push(InputEvent {
                node: NodeId(0),
                tick: Tick(t),
                rel: s,
                row: Arc::from(vec![Value::Str(texts[(t as usize) % texts.len()].into())]),
            });
        }
        let run = differential(&artifact, &inputs, last);
        for (t, rows) in per_tick.iter().enumerate() {
            let instance = &run.node_tick(Tick(t as u64), NodeId(0)).unwrap().instance;
            for view in BYTE_VIEWS {
                let rel = artifact.rel_named(view).unwrap();
                let got: BTreeSet<Vec<Value>> = instance.rows(rel).map(|r| r.to_vec()).collect();
                let want: BTreeSet<Vec<Value>> = rows.iter().map(|r| bytes_expected(view, r)).collect();
                assert_eq!(got, want, "seed {seed} tick {t} view {view}");
                checked += want.len();
            }
            if t >= 1 && t < last as usize {
                let text = texts[t % texts.len()];
                let rows_of = |name: &str| -> Vec<Vec<Value>> {
                    instance
                        .rows(artifact.rel_named(name).unwrap())
                        .map(|r| r.to_vec())
                        .collect()
                };
                let tv = Value::Str(text.into());
                assert_eq!(rows_of("v_text"), vec![vec![tv.clone(), bytes(text.as_bytes())]]);
                assert_eq!(rows_of("v_text_trip"), vec![vec![tv.clone(), opt(Some(tv))]]);
                assert_eq!(rows_of("v_empty"), vec![vec![u(0)]]);
            }
        }
    }
    assert!(checked > 1500, "only {checked} rows checked");
}
