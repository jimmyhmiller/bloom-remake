//! Slice 7's gate: the Blossom Kafka broker (`examples/kafka/broker.bls`) run by `blossom run` never loses an
//! acknowledged record when killed with `kill -9`.
//!
//! A producer (requests encoded and answers decoded by `kafka-protocol`, decision K5) sends batches with acks -1 to
//! three partitions over TCP while the test SIGKILLs the broker process at irregular moments and restarts it from its
//! store. A produce whose connection died unanswered may or may not have landed. Afterwards every partition is read
//! from its start: offsets are consecutive, every acknowledged batch is there at its acknowledged offset with its
//! records, nothing appears twice, and nothing appears that was never sent.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    CreateTopicsRequest, CreateTopicsResponse, ListOffsetsRequest, ListOffsetsResponse, ProduceRequest,
    ProduceResponse, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};

#[cfg(test)]
const TOPIC: &str = "kill9";
#[cfg(test)]
const PARTITIONS: i32 = 3;
#[cfg(test)]
const NOT_LEADER_OR_FOLLOWER: i16 = 6;
#[cfg(test)]
const REQUEST_TIMED_OUT: i16 = 7;

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[cfg(test)]
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a one-broker deployment into `dir`, with small checkpoints so recoveries go through layered checkpoints
/// and blob collection.
#[cfg(test)]
fn deployment(dir: &Path, port: u16) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/kafka/broker.bls");
    let spec = format!(
        r#"format = 1

[deployment]
id = "kafka-kill9"
program = "kafka"
version = 1
source = "{}"
secrets = "k.secrets"

[[node]]
name = "b1"
role = "Broker"
addr = "127.0.0.1:{}"
principal = "spiffe://test/kafka/b1"
streams = {{ kafka = "127.0.0.1:{port}" }}

[statics]
broker = [["b1", 1, "127.0.0.1", {port}]]

[security]
mode = "insecure-dev"

[storage]
data_dir = "data"
checkpoint_wal_bytes = 65536
"#,
        source.display(),
        free_port(),
    );
    let path = dir.join("deploy.toml");
    std::fs::write(&path, spec).unwrap();
    let secrets = dir.join("k.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// Starts `blossom run` and waits for its readiness line.
#[cfg(test)]
fn start(deploy: &Path, fresh: bool) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
    cmd.args(["run", "--deploy"])
        .arg(deploy)
        .args(["--node", "b1", "--insecure-dev"]);
    if fresh {
        cmd.arg("--init-fresh");
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("ready"), "the broker did not come up: {line:?}");
    child
}

/// The broker process, killed when dropped, so a failing test leaves no broker behind.
#[cfg(test)]
struct Broker(Child);

#[cfg(test)]
impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("kill9".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

/// Sends one request and reads its answer's body (after the size prefix); an I/O error is a dead connection.
#[cfg(test)]
fn call(s: &mut TcpStream, frame: &[u8]) -> std::io::Result<Bytes> {
    s.write_all(frame)?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body)?;
    Ok(Bytes::from(body))
}

#[cfg(test)]
fn connect(port: u16) -> std::io::Result<TcpStream> {
    let s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    Ok(s)
}

#[cfg(test)]
fn topic_name() -> TopicName {
    TopicName(StrBytes::from_string(TOPIC.into()))
}

/// The wall clock in milliseconds: records carry the time they are produced, as a client's do, since the broker's
/// time retention judges a segment by its newest record's timestamp against its own clock.
#[cfg(test)]
#[allow(clippy::disallowed_methods)] // a test producer stamps records like a real one
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

#[cfg(test)]
fn batch(values: &[String]) -> Vec<u8> {
    let now = now_ms();
    let records: Vec<Record> = values
        .iter()
        .enumerate()
        .map(|(i, v)| Record {
            transactional: false,
            control: false,
            delete_horizon: false,
            partition_leader_epoch: -1,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: i as i64,
            sequence: i as i32 - 1,
            timestamp: now,
            key: None,
            value: Some(Bytes::from(v.clone())),
            headers: Default::default(),
        })
        .collect();
    let mut buf = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut buf,
        &records,
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap();
    buf.to_vec()
}

/// What the producer learned: acknowledged batches (partition, base offset, values) and values sent unanswered.
#[cfg(test)]
#[derive(Default)]
struct Outcome {
    acked: Vec<(i32, i64, Vec<String>)>,
    unanswered: Vec<Vec<String>>,
}

/// Produces until `stop`, reconnecting whenever the broker dies.
#[cfg(test)]
fn produce(port: u16, stop: Arc<AtomicBool>, out: Arc<Mutex<Outcome>>) {
    let mut conn: Option<TcpStream> = None;
    let mut corr = 0;
    let mut n = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let s = match conn.as_mut() {
            Some(s) => s,
            None => match connect(port) {
                Ok(s) => conn.insert(s),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            },
        };
        corr += 1;
        let p = (n % PARTITIONS as u64) as i32;
        let values: Vec<String> = (0..1 + n % 3).map(|i| format!("r{n}.{i}")).collect();
        n += 1;
        let req = ProduceRequest::default()
            .with_acks(-1)
            .with_timeout_ms(5000)
            .with_topic_data(vec![
                TopicProduceData::default()
                    .with_name(topic_name())
                    .with_partition_data(vec![
                        PartitionProduceData::default()
                            .with_index(p)
                            .with_records(Some(Bytes::from(batch(&values)))),
                    ]),
            ]);
        let answer = call(s, &framed(0, 12, corr, &req)).and_then(|mut body| {
            ResponseHeader::decode(&mut body, ProduceResponse::header_version(12))
                .and_then(|_| ProduceResponse::decode(&mut body, 12))
                .map_err(|e| std::io::Error::other(e.to_string()))
        });
        let mut o = out.lock().unwrap();
        match answer {
            Ok(r) => {
                let pr = &r.responses[0].partition_responses[0];
                match pr.error_code {
                    0 => o.acked.push((p, pr.base_offset, values)),
                    // Retriable, as stock clients treat them: the partition has no leader yet (a restarted broker
                    // before its election), or the batch's fate is unknown (its leadership lost, or the timeout
                    // passed before the high watermark did). Either way the batch may or may not be in the log.
                    NOT_LEADER_OR_FOLLOWER | REQUEST_TIMED_OUT => {
                        o.unanswered.push(values);
                        drop(o);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    code => panic!("a produce was refused with {code}: {pr:?}"),
                }
            }
            Err(_) => {
                o.unanswered.push(values);
                conn = None;
            }
        }
    }
}

/// Every record of a partition from its start, as (offset, value), read with Fetch until the log end.
#[cfg(test)]
fn read_partition(port: u16, topic_id: [u8; 16], p: i32, end: i64) -> Vec<(i64, String)> {
    use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
    use kafka_protocol::messages::{FetchRequest, FetchResponse};
    let mut s = connect(port).unwrap();
    let mut out = Vec::new();
    let mut next = 0;
    let mut corr = 0;
    while next < end {
        corr += 1;
        let req = FetchRequest::default()
            .with_max_bytes(1 << 20)
            .with_min_bytes(1)
            .with_session_epoch(-1)
            .with_topics(vec![
                FetchTopic::default()
                    .with_topic_id(uuid::Uuid::from_bytes(topic_id))
                    .with_partitions(vec![
                        FetchPartition::default()
                            .with_partition(p)
                            .with_current_leader_epoch(-1)
                            .with_fetch_offset(next)
                            .with_partition_max_bytes(1 << 20),
                    ]),
            ]);
        let mut body = call(&mut s, &framed(1, 17, corr, &req)).unwrap();
        ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).unwrap();
        let r = FetchResponse::decode(&mut body, 17).unwrap();
        let part = &r.responses[0].partitions[0];
        if part.error_code == NOT_LEADER_OR_FOLLOWER {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        assert_eq!(part.error_code, 0, "fetching partition {p} at {next}: {part:?}");
        let mut records = part.records.clone().unwrap_or_default();
        let before = next;
        let recs = if records.is_empty() {
            Vec::new()
        } else {
            RecordBatchDecoder::decode(&mut records).unwrap().records
        };
        for rec in recs {
            if rec.offset < next {
                continue;
            }
            out.push((rec.offset, String::from_utf8(rec.value.unwrap().to_vec()).unwrap()));
            next = rec.offset + 1;
        }
        assert!(
            next > before,
            "partition {p}: nothing at offset {next}, below the log end {end}"
        );
    }
    out
}

#[test]
#[ignore = "full tier"]
fn kill_9_never_loses_an_acknowledged_record() {
    let dir = scratch_dir("kafka-kill9");
    let port = free_port();
    let deploy = deployment(&dir, port);
    let mut broker = Broker(start(&deploy, true));
    // The topic, before the producer starts.
    let mut s = connect(port).unwrap();
    let req = CreateTopicsRequest::default()
        .with_topics(vec![
            CreatableTopic::default()
                .with_name(topic_name())
                .with_num_partitions(PARTITIONS)
                .with_replication_factor(1),
        ])
        .with_timeout_ms(5000);
    let mut body = call(&mut s, &framed(19, 7, 1, &req)).unwrap();
    ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7)).unwrap();
    let created = CreateTopicsResponse::decode(&mut body, 7).unwrap();
    assert_eq!(created.topics[0].error_code, 0);
    let topic_id = *created.topics[0].topic_id.as_bytes();
    drop(s);

    let stop = Arc::new(AtomicBool::new(false));
    let out = Arc::new(Mutex::new(Outcome::default()));
    let producer = {
        let (stop, out) = (stop.clone(), out.clone());
        std::thread::spawn(move || produce(port, stop, out))
    };
    // Kill and restart the broker at irregular intervals while the producer runs.
    let mut kills = 0;
    for k in 0..8u64 {
        std::thread::sleep(Duration::from_millis(500 + (k * 373) % 700));
        broker.0.kill().unwrap();
        broker.0.wait().unwrap();
        kills += 1;
        broker = Broker(start(&deploy, false));
    }
    // Produce on after the last restart until enough was acknowledged to make the check meaningful (how many fit in
    // the kill loop depends on the machine's load), within a deadline.
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..600 {
        if out.lock().unwrap().acked.len() > 100 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::SeqCst);
    producer.join().unwrap();

    // Read every partition to its log end.
    let o = out.lock().unwrap();
    assert!(o.acked.len() > 100, "only {} produces were acknowledged", o.acked.len());
    let mut s = connect(port).unwrap();
    let req = ListOffsetsRequest::default()
        .with_replica_id(kafka_protocol::messages::BrokerId(-1))
        .with_topics(vec![
            kafka_protocol::messages::list_offsets_request::ListOffsetsTopic::default()
                .with_name(topic_name())
                .with_partitions(
                    (0..PARTITIONS)
                        .map(|p| {
                            kafka_protocol::messages::list_offsets_request::ListOffsetsPartition::default()
                                .with_partition_index(p)
                                .with_current_leader_epoch(-1)
                                .with_timestamp(-1)
                        })
                        .collect(),
                ),
        ]);
    // Asked again while a partition has no leader yet.
    let mut ends: Vec<i64> = Vec::new();
    for corr in 1..=200 {
        let mut body = call(&mut s, &framed(2, 9, corr, &req)).unwrap();
        ResponseHeader::decode(&mut body, ListOffsetsResponse::header_version(9)).unwrap();
        let parts = ListOffsetsResponse::decode(&mut body, 9).unwrap().topics[0]
            .partitions
            .clone();
        if parts.iter().any(|p| p.error_code == NOT_LEADER_OR_FOLLOWER) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        assert!(parts.iter().all(|p| p.error_code == 0), "listing offsets: {parts:?}");
        ends = parts.iter().map(|p| p.offset).collect();
        break;
    }
    assert_eq!(
        ends.len(),
        PARTITIONS as usize,
        "no partition leaders after the last restart"
    );
    let sent: BTreeSet<String> = o
        .acked
        .iter()
        .flat_map(|a| a.2.clone())
        .chain(o.unanswered.iter().flatten().cloned())
        .collect();
    let mut seen = BTreeSet::new();
    for p in 0..PARTITIONS {
        let log = read_partition(port, topic_id, p, ends[p as usize]);
        // Offsets consecutive from 0; every value sent, and once.
        for (i, (off, v)) in log.iter().enumerate() {
            assert_eq!(*off, i as i64, "partition {p}: a gap before offset {off}");
            assert!(sent.contains(v), "partition {p}: {v} was never sent");
            assert!(seen.insert(v.clone()), "{v} appears twice");
        }
        let at: BTreeMap<i64, &String> = log.iter().map(|(o, v)| (*o, v)).collect();
        for (q, base, values) in o.acked.iter().filter(|a| a.0 == p) {
            for (i, v) in values.iter().enumerate() {
                assert_eq!(
                    at.get(&(base + i as i64)).copied(),
                    Some(v),
                    "partition {q}: acknowledged {v} lost"
                );
            }
        }
    }
    drop(broker);
    assert_eq!(kills, 8);
    let _ = std::fs::remove_dir_all(&dir);
}
