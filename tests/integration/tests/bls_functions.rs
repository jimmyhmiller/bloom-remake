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
        "v_arms" => (
            u(n),
            tuple(vec![u(if n.is_multiple_of(2) { 2 * n } else { 7 + n }), vec_u([n, n + 1, 0, 0])]),
        ),
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
    "v_arms",
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
        "v_bits" => vec![tuple(vec![
            iv(IntValue::U8, r.a & 15),
            iv(IntValue::I8, r.c | 1),
            iv(IntValue::U16, r.d ^ 0xffff),
            iv(IntValue::I16, r.g >> 3),
            iv(IntValue::U32, r.h << 5),
            iv(IntValue::I32, r.i >> 31),
            iv(IntValue::U64, r.j >> 63),
            iv(IntValue::I64, !r.k),
            iv(IntValue::I64, (r.k << 1) ^ (r.k >> 63)),
        ])],
        "v_utf8" => vec![bytes(b), opt(std::str::from_utf8(b).ok().map(|s| Value::Str(s.into())))],
        other => panic!("no expectation for {other}"),
    }
}

#[cfg(test)]
const BYTE_VIEWS: &[&str] = &[
    "v_bits",
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

// ---------------------------------------------------------------- host functions (FOREIGN-PROTOCOLS §4)

#[cfg(test)]
fn std_externs() -> Arc<blossom_value::ExternRegistry> {
    Arc::new(blossom_std_host::registry().unwrap())
}

/// Runs `artifact` with the standard host functions on the oracle and on the engine. They must agree: every tick
/// the same instance, or the same program error at the same tick. Returns the oracle's result.
#[cfg(test)]
enum Hosted {
    Ran(SyncRun),
    Failed(Tick, String),
}

#[cfg(test)]
fn differential_hosted(artifact: &BlsArtifact, inputs: &[InputEvent], last: u64) -> Hosted {
    let externs = std_externs();
    let sim = BlsSim::with_externs(artifact, blossom_value::Seed::from_u64(0), externs.clone()).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim.run(inputs, Tick(last), round, &FaultSchedule::default(), false);
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        externs,
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim.run_on(&engine, inputs, Tick(last), round, &FaultSchedule::default(), false);
    let failure = |r: &Result<SyncRun, blossom_sim::sync::SimError>| match r {
        Ok(_) => None,
        Err(blossom_sim::sync::SimError::Node {
            tick,
            error: blossom_oracle::OracleError::Program { error, .. },
            ..
        }) => Some((*tick, error.code.to_string())),
        Err(e) => panic!("not a program error: {e}"),
    };
    assert_eq!(
        failure(&reference),
        failure(&mine),
        "the oracle and the engine fail differently"
    );
    if let (Ok(a), Ok(b)) = (&reference, &mine) {
        assert_eq!(a.rounds.len(), b.rounds.len());
        for (t, (ra, rb)) in a.rounds.iter().zip(&b.rounds).enumerate() {
            for (x, y) in ra.iter().zip(rb) {
                assert_eq!(x.instance, y.instance, "tick {t}: the oracle and the engine differ");
            }
        }
    }
    match failure(&reference) {
        Some((tick, code)) => Hosted::Failed(tick, code),
        None => Hosted::Ran(reference.unwrap()),
    }
}

#[test]
fn host_functions_run_on_both_evaluators_with_known_answers() {
    let artifact = compile("externs.bls");
    let e = artifact.rel_named("e").unwrap();
    let mut rng = Rng(3);
    let mut inputs = Vec::new();
    let mut sent: Vec<Vec<Vec<u8>>> = vec![Vec::new(); 8];
    for t in 1..7u64 {
        let mut rows: Vec<Vec<u8>> = Vec::new();
        if t == 1 {
            rows.push(b"123456789".to_vec());
            rows.push(b"abc".to_vec());
            rows.push(Vec::new());
        }
        for _ in 0..3 {
            let n = rng.below(3000) as usize;
            // Compressible: runs of a few symbols.
            let mut b = Vec::with_capacity(n);
            while b.len() < n {
                let byte = if rng.below(5) == 0 {
                    rng.next() as u8
                } else {
                    b'x' + rng.below(3) as u8
                };
                b.extend(std::iter::repeat_n(byte, 1 + rng.below(12) as usize));
            }
            b.truncate(n);
            rows.push(b);
        }
        for b in rows {
            inputs.push(InputEvent {
                node: NodeId(0),
                tick: Tick(t),
                rel: e,
                row: Arc::from(vec![bytes(&b)]),
            });
            sent[t as usize].push(b);
        }
    }
    let Hosted::Ran(run) = differential_hosted(&artifact, &inputs, 7) else {
        panic!("the host functions failed");
    };
    let hex = |v: &Value| match v {
        Value::Bytes(x) => x.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        other => panic!("{other:?}"),
    };
    let mut checked = 0;
    for (t, rows) in sent.iter().enumerate() {
        let instance = &run.node_tick(Tick(t as u64), NodeId(0)).unwrap().instance;
        let rows_of = |name: &str| -> Vec<Vec<Value>> {
            instance
                .rows(artifact.rel_named(name).unwrap())
                .map(|r| r.to_vec())
                .collect()
        };
        let trips = rows_of("v_trips");
        assert_eq!(trips.len(), rows.iter().collect::<BTreeSet<_>>().len(), "tick {t}");
        for r in &trips {
            let Value::Bytes(b) = &r[0] else { panic!() };
            // Every codec round-trips at the exact bound and refuses one byte less; the input itself is no valid
            // compressed stream.
            let want = tuple(vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
            ]);
            assert_eq!(r[1], want, "tick {t}: {} bytes", b.len());
            checked += 1;
        }
        for r in rows_of("v_crc") {
            if r[0] == bytes(b"123456789") {
                assert_eq!(r[1], iv(IntValue::U32, 0xe306_9283));
                assert_eq!(r[2], iv(IntValue::U32, 0xcbf4_3926));
                checked += 1;
            }
        }
        for r in rows_of("v_hash") {
            if r[0] == bytes(b"abc") {
                assert_eq!(
                    hex(&r[1]),
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                );
                checked += 1;
            }
            if r[0] == bytes(b"") {
                assert_eq!(
                    hex(&r[2]),
                    "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= 20, "only {checked} checks");
}

#[test]
fn a_host_function_that_refuses_its_input_is_blsr010_on_both_evaluators() {
    let artifact = compile("externs.bls");
    let bad = artifact.rel_named("bad_level").unwrap();
    let level = |t: u64, l: u8| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: bad,
        row: Arc::from(vec![iv(IntValue::U8, l)]),
    };
    assert!(
        matches!(differential_hosted(&artifact, &[level(1, 9)], 3), Hosted::Ran(_)),
        "level 9 is valid"
    );
    assert!(matches!(
        differential_hosted(&artifact, &[level(1, 9), level(2, 10)], 3),
        Hosted::Failed(Tick(2), code) if code == "BLSR010"
    ));
}

#[test]
fn a_fold_evaluates_its_receiver_before_its_initial_value_on_both_evaluators() {
    let artifact = compile("externs.bls");
    let rel = artifact.rel_named("fold_order").unwrap();
    let input = InputEvent {
        node: NodeId(0),
        tick: Tick(1),
        rel,
        row: Arc::from(vec![u(5)]),
    };
    assert!(matches!(
        differential_hosted(&artifact, &[input], 2),
        Hosted::Failed(Tick(1), code) if code == "BLSR004"
    ));
}

#[test]
fn a_function_past_its_step_budget_is_blsr012_on_both_evaluators() {
    let artifact = compile("budget.bls");
    let at = |rel: &str, t: u64, n: u64| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: artifact.rel_named(rel).unwrap(),
        row: Arc::from(vec![u(n)]),
    };
    let budget = blossom_ir::core::FN_STEP_BUDGET;
    // Within the budget: exactly at it, and a nested evaluation just under it.
    match differential_hosted(
        &artifact,
        &[at("spin", 1, budget), at("big", 1, budget), at("nest", 1, 3000)],
        2,
    ) {
        Hosted::Ran(run) => {
            let instance = &run.node_tick(Tick(1), NodeId(0)).unwrap().instance;
            let rows = |v: &str| -> Vec<Vec<Value>> {
                instance
                    .rows(artifact.rel_named(v).unwrap())
                    .map(|r| r.to_vec())
                    .collect()
            };
            assert_eq!(rows("v_spin"), vec![vec![u(budget), u(budget)]]);
            assert_eq!(rows("v_big"), vec![vec![u(budget), u(budget)]]);
            assert_eq!(rows("v_nest"), vec![vec![u(3000), u(3000)]]);
        }
        Hosted::Failed(t, code) => panic!("within the budget, yet {code} at {t:?}"),
    }
    // One step over, a range of 2⁶² elements (never allocated), and a nested evaluation over it.
    for input in [at("spin", 1, budget + 1), at("big", 1, 1 << 62), at("nest", 1, 4000)] {
        assert!(matches!(
            differential_hosted(&artifact, &[input], 2),
            Hosted::Failed(Tick(1), code) if code == "BLSR012"
        ));
    }
}

#[test]
fn a_program_whose_host_functions_are_not_registered_does_not_load() {
    let artifact = compile("externs.bls");
    let listed = |e: &blossom_oracle::OracleError| match e {
        blossom_oracle::OracleError::Externs(problems) => problems.len(),
        other => panic!("expected the unbound host functions, got {other}"),
    };
    match BlsSim::new(&artifact, blossom_value::Seed::from_u64(0)) {
        Err(blossom_sim::sync::SimError::Load(e)) => assert_eq!(listed(&e), 12),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("the oracle loaded a program with unbound host functions"),
    }
    match blossom_engine::Engine::new(
        artifact.program.clone(),
        NodeId(0),
        blossom_engine::EngineConfig::default(),
    ) {
        Err(e) => assert_eq!(listed(&e), 12),
        Ok(_) => panic!("the engine loaded a program with unbound host functions"),
    }
    // A registry that has the paths under other signatures is refused as well.
    let mut wrong = blossom_value::ExternRegistry::new();
    for x in blossom_value::STD_EXTERNS {
        wrong
            .register_typed_fn(x.path, vec![blossom_value::HostType::Str], x.ret, |_: &[Value]| {
                Ok(Value::Unit)
            })
            .unwrap();
    }
    match BlsSim::with_externs(&artifact, blossom_value::Seed::from_u64(0), Arc::new(wrong)) {
        Err(blossom_sim::sync::SimError::Load(e)) => assert_eq!(listed(&e), 12),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("loaded against mismatched host signatures"),
    }
}

#[test]
fn a_shift_outside_the_width_is_blsr004_on_both_evaluators() {
    let artifact = compile("shift.bls");
    let e = artifact.rel_named("e").unwrap();
    let at = |t: u64, n: u32| InputEvent {
        node: NodeId(0),
        tick: Tick(t),
        rel: e,
        row: Arc::from(vec![iv(IntValue::U32, n)]),
    };
    let Hosted::Ran(run) = differential_hosted(&artifact, &[at(1, 31)], 2) else {
        panic!("a shift by 31 of a u32 is in range");
    };
    let v = artifact.rel_named("v").unwrap();
    let rows: Vec<Vec<Value>> = run
        .node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(v)
        .map(|r| r.to_vec())
        .collect();
    assert_eq!(rows, vec![vec![iv(IntValue::U32, 1 << 31)]]);
    assert!(matches!(
        differential_hosted(&artifact, &[at(1, 31), at(2, 32)], 3),
        Hosted::Failed(Tick(2), code) if code == "BLSR004"
    ));
}
