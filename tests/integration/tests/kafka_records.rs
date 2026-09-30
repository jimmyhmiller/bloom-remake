//! Slice 7, item 3: record batches in Blossom (`examples/kafka/records.bls`) against an independent implementation
//! (the `kafka-protocol` crate's encoder and decoder). Random batches, uncompressed and compressed, are split, checked
//! and given offsets in Blossom on both evaluators: a corrupt byte fails the CRC32C, a truncated field is malformed,
//! and an assigned batch decodes with its new offsets and a checksum that still holds, the rest of its bytes
//! unchanged.

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
use bytes::{BufMut, Bytes, BytesMut};
use kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};

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
    fn bytes(&mut self, most: u64) -> Vec<u8> {
        (0..self.below(most)).map(|_| self.next() as u8).collect()
    }
}

/// A random batch of `n` records from offset 0, `compressed` or not (a compressed batch's records are passed through
/// as they are: the broker never looks inside).
#[cfg(test)]
fn batch(rng: &mut Rng, n: usize, compressed: bool) -> Vec<u8> {
    let records: Vec<Record> = (0..n)
        .map(|i| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            // A producer without idempotence sends base sequence -1; the encoder derives the rest from it.
            sequence: i as i32 - 1,
            timestamp: 1_700_000_000_000 + rng.below(1000) as i64,
            key: if rng.below(2) == 0 { None } else { Some(Bytes::from(rng.bytes(20))) },
            value: Some(Bytes::from(rng.bytes(200))),
            headers: Default::default(),
        })
        .collect();
    let mut buf = BytesMut::new();
    let options = RecordEncodeOptions {
        version: 2,
        compression: if compressed { Compression::Gzip } else { Compression::None },
    };
    RecordBatchEncoder::encode_with_custom_compression(
        &mut buf,
        &records,
        &options,
        Some(|input: &mut BytesMut, out: &mut BytesMut, _c: Compression| {
            out.put_slice(input);
            Ok(())
        }),
    )
    .unwrap();
    buf.to_vec()
}

#[cfg(test)]
fn compile() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/kafka/records.bls");
    let (result, _) = compile_file(
        path.to_str().unwrap(),
        &[NodeSpec {
            name: "n1".to_owned(),
            role: None,
        }],
    );
    result.unwrap_or_else(|e| panic!("records.bls: {e:?}")).0
}

/// Runs the harness on the oracle and the engine (which must agree), with the standard host functions.
#[cfg(test)]
fn run(artifact: &BlsArtifact, inputs: &[InputEvent]) -> SyncRun {
    let externs = Arc::new(blossom_std_host::registry().unwrap());
    let sim = BlsSim::with_externs(artifact, blossom_value::Seed::from_u64(0), externs.clone()).unwrap();
    let round = Duration::from_nanos(1_000_000_000);
    let reference = sim
        .run(inputs, Tick(1), round, &FaultSchedule::default(), false)
        .unwrap();
    let cfg = blossom_engine::EngineConfig {
        roles: artifact.roles.clone(),
        node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
        seed: Some(blossom_value::Seed::from_u64(0)),
        externs,
        ..blossom_engine::EngineConfig::default()
    };
    let engine = EngineEvaluator::new(artifact.program.clone(), cfg);
    let mine = sim
        .run_on(&engine, inputs, Tick(1), round, &FaultSchedule::default(), false)
        .unwrap();
    for (t, (a, b)) in reference.rounds.iter().zip(&mine.rounds).enumerate() {
        assert_eq!(a[0].instance, b[0].instance, "tick {t}: the oracle and the engine differ");
    }
    reference
}

#[cfg(test)]
fn u(x: u64) -> Value {
    Value::Int(IntValue::U64(x))
}

#[cfg(test)]
fn bytes(b: &[u8]) -> Value {
    Value::Bytes(Arc::from(b))
}

#[cfg(test)]
fn rows(artifact: &BlsArtifact, run: &SyncRun, view: &str) -> BTreeSet<Vec<Value>> {
    run.node_tick(Tick(1), NodeId(0))
        .unwrap()
        .instance
        .rows(artifact.rel_named(view).unwrap())
        .map(|r| r.to_vec())
        .collect()
}

#[test]
fn batches_split_check_and_take_offsets_as_kafka_reads_them() {
    let artifact = compile();
    let (field, assign) = (artifact.rel_named("field").unwrap(), artifact.rel_named("assign").unwrap());
    let mut rng = Rng(7);
    let mut inputs = Vec::new();
    // Per field: its id, the number of batches, and which are sound (`None`: malformed).
    let mut want_split: Vec<(u64, Option<Vec<bool>>)> = Vec::new();
    let mut assigns: Vec<(u64, Vec<u8>, i64, i32)> = Vec::new();
    for id in 0..60u64 {
        let n = 1 + rng.below(3) as usize;
        let mut batches: Vec<Vec<u8>> = (0..n)
            .map(|_| {
                let recs = 1 + rng.below(5) as usize;
                let compressed = rng.below(2) == 0;
                batch(&mut rng, recs, compressed)
            })
            .collect();
        let mut sound = vec![true; n];
        match rng.below(4) {
            // Corrupt a byte the CRC covers.
            0 => {
                let i = rng.below(n as u64) as usize;
                let b = &mut batches[i];
                let at = 21 + rng.below(b.len() as u64 - 21) as usize;
                b[at] ^= 1 << rng.below(8);
                sound[i] = false;
            }
            // Truncate the field.
            1 => {
                let last = batches.last_mut().unwrap();
                last.truncate(last.len() - 1 - rng.below(10) as usize);
                let field_bytes: Vec<u8> = batches.concat();
                inputs.push(InputEvent {
                    node: NodeId(0),
                    tick: Tick(1),
                    rel: field,
                    row: Arc::from(vec![u(id), bytes(&field_bytes)]),
                });
                want_split.push((id, None));
                continue;
            }
            _ => {}
        }
        for (b, ok) in batches.iter().zip(&sound) {
            if *ok && rng.below(2) == 0 {
                let (base, epoch) = (rng.below(1 << 40) as i64, rng.below(1000) as i32);
                assigns.push((id * 10 + assigns.len() as u64, b.clone(), base, epoch));
            }
        }
        inputs.push(InputEvent {
            node: NodeId(0),
            tick: Tick(1),
            rel: field,
            row: Arc::from(vec![u(id), bytes(&batches.concat())]),
        });
        want_split.push((id, Some(sound)));
    }
    for (id, b, base, epoch) in &assigns {
        inputs.push(InputEvent {
            node: NodeId(0),
            tick: Tick(1),
            rel: assign,
            row: Arc::from(vec![
                u(*id),
                bytes(b),
                Value::Int(IntValue::I64(*base)),
                Value::Int(IntValue::I32(*epoch)),
            ]),
        });
    }
    let r = run(&artifact, &inputs);
    let oks = rows(&artifact, &r, "v_ok");
    for (id, sound) in &want_split {
        let want = match sound {
            None => Value::Option(None),
            Some(s) => Value::some(Value::Vec(s.iter().map(|x| Value::Bool(*x)).collect())),
        };
        let got = oks.iter().find(|r| r[0] == u(*id));
        assert!(oks.contains(&vec![u(*id), want.clone()]), "field {id}: want {want:?}, got {got:?}");
    }
    let assigned = rows(&artifact, &r, "v_assigned");
    for (id, b, base, epoch) in &assigns {
        let row = assigned.iter().find(|x| x[0] == u(*id)).unwrap();
        let Value::Option(Some(out)) = &row[1] else { panic!("{row:?}") };
        let Value::Bytes(out) = &**out else { panic!("{out:?}") };
        // Only the base offset and the leader epoch changed.
        assert_eq!(&out[..8], &base.to_be_bytes());
        assert_eq!(&out[12..16], &epoch.to_be_bytes());
        assert_eq!(&out[8..12], &b[8..12]);
        assert_eq!(&out[16..], &b[16..]);
        // An uncompressed batch decodes (checksum included) with its records at the new offsets.
        let compressed = i16::from_be_bytes([b[21], b[22]]) & 7 != 0;
        if !compressed {
            let set = RecordBatchDecoder::decode(&mut Bytes::copy_from_slice(out)).unwrap();
            for (i, rec) in set.records.iter().enumerate() {
                assert_eq!(rec.offset, base + i as i64, "batch {id}");
                assert_eq!(rec.partition_leader_epoch, *epoch);
            }
        }
    }
}
