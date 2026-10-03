//! Slice 8, item 7: the Blossom Kafka broker (`examples/kafka/broker.bls`) as three `blossom run` processes, a
//! replicated cluster over TCP.
//!
//! Every link between brokers goes through a proxy the test controls, so the network partitions for real. Producers
//! (`kafka-protocol`, decision K5) learn each partition's leader from Metadata and send it batches with acks -1,
//! refreshing their metadata when a leader moves; a nemesis kills brokers with SIGKILL (and restarts them), isolates
//! one, and heals. Afterwards every partition is read from its leader: offsets consecutive, every acknowledged batch
//! at its offset, nothing twice, nothing never sent. Then each partition's leader is killed and the partition read
//! again from the next: the same records, as far as the first read went (a committed record is on a majority).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_bench::stopwatch::Stopwatch;

use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::create_topics_request::CreatableTopic;
use kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};
use kafka_protocol::messages::list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic};
use kafka_protocol::messages::metadata_request::MetadataRequestTopic;
use kafka_protocol::messages::produce_request::{PartitionProduceData, TopicProduceData};
use kafka_protocol::messages::{
    BrokerId, CreateTopicsRequest, CreateTopicsResponse, FetchRequest, FetchResponse, ListOffsetsRequest,
    ListOffsetsResponse, MetadataRequest, MetadataResponse, ProduceRequest, ProduceResponse, RequestHeader,
    ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, HeaderVersion, StrBytes};
use kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};

#[cfg(test)]
const TOPIC: &str = "replicated";
#[cfg(test)]
const PARTITIONS: i32 = 3;
#[cfg(test)]
const NOT_LEADER_OR_FOLLOWER: i16 = 6;
#[cfg(test)]
const REQUEST_TIMED_OUT: i16 = 7;
#[cfg(test)]
const LEADER_NOT_AVAILABLE: i16 = 5;

#[cfg(test)]
fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

/// A TCP proxy for one directed link between brokers: forwards connections to `upstream` unless blocked. Blocking
/// cuts the open connections and refuses new ones (as in `raft3.rs`).
#[cfg(test)]
struct Proxy {
    addr: SocketAddr,
    state: Arc<Mutex<(bool, Vec<TcpStream>)>>,
}

#[cfg(test)]
impl Proxy {
    fn start(upstream: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state: Arc<Mutex<(bool, Vec<TcpStream>)>> = Arc::new(Mutex::new((false, Vec::new())));
        let st = state.clone();
        std::thread::spawn(move || {
            for inbound in listener.incoming() {
                let Ok(inbound) = inbound else { continue };
                if st.lock().unwrap().0 {
                    drop(inbound);
                    continue;
                }
                let Ok(outbound) = TcpStream::connect(upstream) else {
                    continue;
                };
                let mut reg = st.lock().unwrap();
                if reg.0 {
                    continue;
                }
                reg.1.push(inbound.try_clone().unwrap());
                reg.1.push(outbound.try_clone().unwrap());
                drop(reg);
                let pipe = |mut from: TcpStream, mut to: TcpStream| {
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 64 * 1024];
                        loop {
                            match from.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if to.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    });
                };
                pipe(inbound.try_clone().unwrap(), outbound.try_clone().unwrap());
                pipe(outbound, inbound);
            }
        });
        Proxy { addr, state }
    }

    fn block(&self) {
        let mut st = self.state.lock().unwrap();
        st.0 = true;
        for c in st.1.drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }

    fn heal(&self) {
        self.state.lock().unwrap().0 = false;
    }
}

/// Three brokers: their deployment, processes and the proxies between them.
#[cfg(test)]
struct Cluster {
    dir: PathBuf,
    deploy: PathBuf,
    names: Vec<String>,
    /// Each broker's Kafka port (its broker id is its index plus one).
    ports: Vec<u16>,
    procs: Vec<Option<Child>>,
    proxies: Vec<((usize, usize), Proxy)>,
}

#[cfg(test)]
impl Cluster {
    fn new(tag: &str) -> Cluster {
        Cluster::with_storage(tag, "")
    }

    /// As `new`, with more lines for the deployment's `[storage]` table.
    fn with_storage(tag: &str, storage: &str) -> Cluster {
        Cluster::with_config(tag, "", storage)
    }

    /// As `new`, with more lines for the deployment's `[params]` and `[storage]` tables.
    fn with_config(tag: &str, params: &str, storage: &str) -> Cluster {
        let dir = std::env::temp_dir().join(format!("blossom-kafka3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<String> = (1..=3).map(|i| format!("b{i}")).collect();
        let peer: Vec<SocketAddr> = names.iter().map(|_| free_addr()).collect();
        let ports: Vec<u16> = names.iter().map(|_| free_addr().port()).collect();
        let mut proxies = Vec::new();
        for i in 0..3 {
            for (j, upstream) in peer.iter().enumerate() {
                if i != j {
                    proxies.push(((i, j), Proxy::start(*upstream)));
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/kafka/broker.bls");
        let mut spec = format!(
            "format = 1\n\n[deployment]\nid = \"kafka3\"\nprogram = \"kafka\"\nversion = 1\nsource = \"{}\"\nsecrets = \"k.secrets\"\n",
            source.display()
        );
        for (i, n) in names.iter().enumerate() {
            let dial: Vec<String> = proxies
                .iter()
                .filter(|((a, _), _)| *a == i)
                .map(|((_, b), p)| format!("{} = \"{}\"", names[*b], p.addr))
                .collect();
            spec.push_str(&format!(
                "\n[[node]]\nname = \"{n}\"\nrole = \"Broker\"\naddr = \"{}\"\nprincipal = \"spiffe://test/kafka/{n}\"\nstreams = {{ kafka = \"127.0.0.1:{}\" }}\ndial = {{ {} }}\n",
                peer[i],
                ports[i],
                dial.join(", ")
            ));
        }
        let statics: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, n)| format!("[\"{n}\", {}, \"127.0.0.1\", {}]", i + 1, ports[i]))
            .collect();
        spec.push_str(&format!(
            "\n[params]\nREPLICA_LAG_MAX = \"2s\"\n{params}\n\n[statics]\nbroker = [{}]\n\n[security]\nmode = \"insecure-dev\"\n\n[storage]\ndata_dir = \"data\"\ncheckpoint_wal_bytes = 262144\n{storage}",
            statics.join(", ")
        ));
        let deploy = dir.join("deploy.toml");
        std::fs::write(&deploy, spec).unwrap();
        let secrets = dir.join("k.secrets");
        std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut c = Cluster {
            dir,
            deploy,
            names,
            ports,
            procs: vec![None, None, None],
            proxies,
        };
        for i in 0..3 {
            c.start(i, true);
        }
        c
    }

    fn start(&mut self, i: usize, fresh: bool) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blossom"));
        cmd.args(["run", "--deploy"])
            .arg(&self.deploy)
            .args(["--node", &self.names[i], "--insecure-dev", "--stats"])
            .arg(self.stats_path(i));
        if fresh {
            cmd.arg("--init-fresh");
        }
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.contains("ready"), "{} did not come up: {line:?}", self.names[i]);
        self.procs[i] = Some(child);
    }

    /// Where broker `i` writes its counters (`blossom run --stats`).
    fn stats_path(&self, i: usize) -> PathBuf {
        self.dir.join(format!("{}.stats", self.names[i]))
    }

    /// Broker `i`'s tick count, as its stats file last said (it rewrites the file every second).
    fn ticks(&self, i: usize) -> u64 {
        let text = std::fs::read_to_string(self.stats_path(i)).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("ticks "))
            .unwrap_or_else(|| panic!("no tick count in {text:?}"))
            .parse()
            .unwrap()
    }

    fn kill(&mut self, i: usize) {
        if let Some(mut c) = self.procs[i].take() {
            c.kill().unwrap(); // SIGKILL
            c.wait().unwrap();
        }
    }

    fn isolate(&self, i: usize) {
        for ((a, b), p) in &self.proxies {
            if *a == i || *b == i {
                p.block();
            }
        }
    }

    fn heal(&self) {
        for (_, p) in &self.proxies {
            p.heal();
        }
    }
}

#[cfg(test)]
impl Drop for Cluster {
    fn drop(&mut self) {
        for i in 0..3 {
            self.kill(i);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
fn framed<M: Encodable + HeaderVersion>(key: i16, version: i16, corr: i32, body: &M) -> Vec<u8> {
    let mut buf = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key)
        .with_request_api_version(version)
        .with_correlation_id(corr)
        .with_client_id(Some(StrBytes::from_string("kafka3".into())))
        .encode(&mut buf, M::header_version(version))
        .unwrap();
    body.encode(&mut buf, version).unwrap();
    let mut out = (buf.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&buf);
    out
}

/// Sends one request and reads its answer's body; an I/O error is a dead connection (or a timeout).
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
    s.set_read_timeout(Some(Duration::from_secs(4)))?;
    Ok(s)
}

#[cfg(test)]
fn topic_name() -> TopicName {
    TopicName(StrBytes::from_string(TOPIC.into()))
}

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

/// The topic's id and each partition's leader's Kafka port, as broker `port` reports them (`None` while it cannot
/// say: it is down, or knows no leader for some partition).
#[cfg(test)]
fn metadata(ports: &[u16], port: u16) -> Option<([u8; 16], BTreeMap<i32, u16>)> {
    let mut s = connect(port).ok()?;
    let req = MetadataRequest::default()
        .with_topics(Some(vec![
            MetadataRequestTopic::default().with_name(Some(topic_name())),
        ]))
        .with_allow_auto_topic_creation(false);
    let mut body = call(&mut s, &framed(3, 13, 1, &req)).ok()?;
    ResponseHeader::decode(&mut body, MetadataResponse::header_version(13)).ok()?;
    let r = MetadataResponse::decode(&mut body, 13).ok()?;
    let t = r.topics.first().filter(|t| t.error_code == 0)?;
    let mut leaders = BTreeMap::new();
    for p in &t.partitions {
        let l = p.leader_id.0;
        if !(1..=3).contains(&l) {
            return None;
        }
        leaders.insert(p.partition_index, ports[(l - 1) as usize]);
    }
    (leaders.len() == PARTITIONS as usize).then_some((*t.topic_id.as_bytes(), leaders))
}

/// What producers learned: acknowledged batches (partition, base offset, values) and values whose fate is unknown.
#[cfg(test)]
#[derive(Default)]
struct Outcome {
    acked: Vec<(i32, i64, Vec<String>)>,
    unknown: Vec<Vec<String>>,
}

/// Produces until `stop`, following leaders.
#[cfg(test)]
fn produce(id: usize, ports: Vec<u16>, stop: Arc<AtomicBool>, out: Arc<Mutex<Outcome>>) {
    let mut leaders: BTreeMap<i32, u16> = BTreeMap::new();
    let mut conns: BTreeMap<u16, TcpStream> = BTreeMap::new();
    let mut n = 0u64;
    let mut corr = 0;
    while !stop.load(Ordering::SeqCst) {
        if leaders.len() < PARTITIONS as usize {
            let port = ports[(n as usize + id) % ports.len()];
            n += 1;
            match metadata(&ports, port) {
                Some((_, l)) => leaders = l,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
            continue;
        }
        let p = (n % PARTITIONS as u64) as i32;
        let port = leaders[&p];
        let values: Vec<String> = (0..1 + n % 3).map(|i| format!("p{id}.{n}.{i}")).collect();
        n += 1;
        corr += 1;
        let req = ProduceRequest::default()
            .with_acks(-1)
            .with_timeout_ms(3000)
            .with_topic_data(vec![
                TopicProduceData::default()
                    .with_name(topic_name())
                    .with_partition_data(vec![
                        PartitionProduceData::default()
                            .with_index(p)
                            .with_records(Some(Bytes::from(batch(&values)))),
                    ]),
            ]);
        let frame = framed(0, 12, corr, &req);
        let s = match conns.get_mut(&port) {
            Some(s) => s,
            None => match connect(port) {
                Ok(s) => conns.entry(port).or_insert(s),
                Err(_) => {
                    leaders.clear();
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            },
        };
        let answer = call(s, &frame).and_then(|mut body| {
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
                    NOT_LEADER_OR_FOLLOWER | REQUEST_TIMED_OUT | LEADER_NOT_AVAILABLE => {
                        o.unknown.push(values);
                        leaders.clear();
                    }
                    code => panic!("a produce was refused with {code}: {pr:?}"),
                }
            }
            Err(_) => {
                o.unknown.push(values);
                conns.remove(&port);
                leaders.clear();
            }
        }
    }
}

/// The topic, created with three replicas through any broker that answers.
#[cfg(test)]
fn create_topic(ports: &[u16]) {
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(30));
    for k in 0.. {
        assert!(clock.elapsed() < limit, "the topic could not be created");
        let Ok(mut s) = connect(ports[k % 3]) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let req = CreateTopicsRequest::default()
            .with_topics(vec![
                CreatableTopic::default()
                    .with_name(topic_name())
                    .with_num_partitions(PARTITIONS)
                    .with_replication_factor(3),
            ])
            .with_timeout_ms(5000);
        let Ok(mut body) = call(&mut s, &framed(19, 7, 1, &req)) else {
            continue;
        };
        ResponseHeader::decode(&mut body, CreateTopicsResponse::header_version(7)).unwrap();
        let r = CreateTopicsResponse::decode(&mut body, 7).unwrap();
        match r.topics[0].error_code {
            0 | 36 => return,
            REQUEST_TIMED_OUT => continue,
            code => panic!("creating the topic answered {code}"),
        }
    }
}

/// Partition `p` at the broker on `port` (its leader) from offset 0 to its high watermark, as (offset, value);
/// `None` if the broker cannot answer (not the leader any more, down).
#[cfg(test)]
fn read_partition(port: u16, topic_id: [u8; 16], p: i32) -> Option<Vec<(i64, String)>> {
    let mut s = connect(port).ok()?;
    let req = ListOffsetsRequest::default()
        .with_replica_id(BrokerId(-1))
        .with_topics(vec![
            ListOffsetsTopic::default()
                .with_name(topic_name())
                .with_partitions(vec![
                    ListOffsetsPartition::default()
                        .with_partition_index(p)
                        .with_current_leader_epoch(-1)
                        .with_timestamp(-1),
                ]),
        ]);
    let mut body = call(&mut s, &framed(2, 9, 1, &req)).ok()?;
    ResponseHeader::decode(&mut body, ListOffsetsResponse::header_version(9)).ok()?;
    let lo = ListOffsetsResponse::decode(&mut body, 9).ok()?;
    let part = &lo.topics[0].partitions[0];
    if part.error_code != 0 {
        return None;
    }
    let end = part.offset;
    let mut out = Vec::new();
    let mut next = 0;
    let mut corr = 1;
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
        let mut body = call(&mut s, &framed(1, 17, corr, &req)).ok()?;
        ResponseHeader::decode(&mut body, FetchResponse::header_version(17)).ok()?;
        let r = FetchResponse::decode(&mut body, 17).ok()?;
        let part = &r.responses[0].partitions[0];
        if part.error_code != 0 {
            return None;
        }
        let mut records = part.records.clone().unwrap_or_default();
        let before = next;
        while !records.is_empty() {
            let Ok(batch) = RecordBatchDecoder::decode(&mut records) else {
                break;
            };
            for rec in batch.records {
                if rec.offset < next {
                    continue;
                }
                out.push((rec.offset, String::from_utf8(rec.value.unwrap().to_vec()).unwrap()));
                next = rec.offset + 1;
            }
        }
        assert!(
            next > before,
            "partition {p}: nothing at offset {next}, below the high watermark {end}"
        );
    }
    Some(out)
}

/// Each partition read from its leader (asking the brokers on `live` until every partition has a leader that
/// answers; `ports` are every broker's, by id).
#[cfg(test)]
fn read_all(ports: &[u16], live: &[u16]) -> BTreeMap<i32, Vec<(i64, String)>> {
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(60));
    loop {
        assert!(clock.elapsed() < limit, "the partitions could not all be read");
        let mut logs = BTreeMap::new();
        for port in live {
            if let Some((tid, leaders)) = metadata(ports, *port) {
                for (p, lp) in &leaders {
                    if let Some(log) = read_partition(*lp, tid, *p) {
                        logs.insert(*p, log);
                    }
                }
                break;
            }
        }
        if logs.len() == PARTITIONS as usize {
            return logs;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "full tier"]
fn three_brokers_keep_every_acknowledged_record_under_kill_9_and_partitions() {
    let _one = crate::one_cluster();
    let mut cluster = Cluster::new("rust");
    let ports = cluster.ports.clone();
    create_topic(&ports);
    let stop = Arc::new(AtomicBool::new(false));
    let out = Arc::new(Mutex::new(Outcome::default()));
    let producers: Vec<_> = (0..3)
        .map(|id| {
            let (ports, stop, out) = (ports.clone(), stop.clone(), out.clone());
            std::thread::spawn(move || produce(id, ports, stop, out))
        })
        .collect();
    // The nemesis: kills (a broker comes back after a while), isolations, heals; with a pause between faults.
    let mut kills = 0;
    for k in 0..10usize {
        std::thread::sleep(Duration::from_millis(700 + (k as u64 * 211) % 600));
        // Kills go round every broker (b1 first: the controller's first leader); isolations too.
        let victim = k % 3;
        match k % 3 {
            0 | 1 => {
                cluster.kill(victim);
                kills += 1;
                std::thread::sleep(Duration::from_millis(300 + (k as u64 * 97) % 700));
                cluster.start(victim, false);
            }
            _ => {
                cluster.isolate(victim);
                std::thread::sleep(Duration::from_millis(1500));
                cluster.heal();
            }
        }
    }
    cluster.heal();
    // Produce on after the faults until enough was acknowledged to make the checks meaningful (how many fit in the
    // nemesis's time depends on the machine's load), within a deadline.
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(90));
    while out.lock().unwrap().acked.len() <= 100 && clock.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::SeqCst);
    for p in producers {
        p.join().unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));

    let o = out.lock().unwrap();
    assert!(o.acked.len() > 100, "only {} produces were acknowledged", o.acked.len());
    let sent: BTreeSet<String> = o
        .acked
        .iter()
        .flat_map(|a| a.2.clone())
        .chain(o.unknown.iter().flatten().cloned())
        .collect();
    let check = |logs: &BTreeMap<i32, Vec<(i64, String)>>| {
        let mut seen = BTreeSet::new();
        for (p, log) in logs {
            for (i, (off, v)) in log.iter().enumerate() {
                assert_eq!(*off, i as i64, "partition {p}: a gap before offset {off}");
                assert!(sent.contains(v), "partition {p}: {v} was never sent");
                assert!(seen.insert(v.clone()), "{v} appears twice");
            }
            let at: BTreeMap<i64, &String> = log.iter().map(|(o, v)| (*o, v)).collect();
            for (q, base, values) in o.acked.iter().filter(|a| a.0 == *p) {
                for (i, v) in values.iter().enumerate() {
                    assert_eq!(
                        at.get(&(base + i as i64)).copied(),
                        Some(v),
                        "partition {q}: acknowledged {v} lost"
                    );
                }
            }
        }
    };
    let first = read_all(&ports, &ports);
    check(&first);
    // Each partition's leader goes; the next one holds the same records, as far as the first read went.
    for victim in 0..3 {
        cluster.kill(victim);
        let up: Vec<u16> = ports
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != victim)
            .map(|(_, p)| *p)
            .collect();
        let again = read_all(&ports, &up);
        check(&again);
        for (p, log) in &first {
            let later = &again[p];
            assert!(
                later.len() >= log.len(),
                "partition {p}: fewer records without broker {}",
                victim + 1
            );
            assert_eq!(
                &later[..log.len()],
                &log[..],
                "partition {p}: other records without broker {}",
                victim + 1
            );
        }
        cluster.start(victim, false);
        std::thread::sleep(Duration::from_secs(2));
    }
    assert_eq!(kills, 7);
    // Idle, a broker ticks only for its timers and its peers' messages: none spins (a leader once rewrote a
    // follower's caught-up time with the clock at every tick, so it was always ready and used a whole core).
    //
    // Counted in ticks, which machine load does not change (CPU time did: HD item 4). An idle broker here ticks about
    // 155 times a second: `raft_poll` (50/s, while it follows a group), `raft_heartbeat` (20/s), and its two peers'
    // heartbeats, acknowledgements and leadership announcements with the tick each one stages. The stats files are
    // rewritten every second, so the 5 s between the reads count 4 to 6 s of ticks: under 1 000. A broker that spins
    // ticks as fast as it computes, about 400 times a second in a debug build: over 2 000. (Under a heavily loaded
    // machine a spinning broker may tick less and pass; an idle one never fails.)
    std::thread::sleep(Duration::from_secs(2));
    let before: Vec<u64> = (0..3).map(|i| cluster.ticks(i)).collect();
    std::thread::sleep(Duration::from_secs(5));
    for (i, b) in before.iter().enumerate() {
        let ticks = cluster.ticks(i) - b;
        assert!(ticks < 1_250, "broker {} ticked {ticks} times in 5 s idle", i + 1);
        // And it is alive, its stats fresh: `raft_heartbeat` alone ticks it 20 times a second.
        assert!(ticks >= 80, "broker {} ticked only {ticks} times in 5 s idle", i + 1);
    }
    for (i, p) in cluster.procs.iter_mut().enumerate() {
        let p = p.as_mut().expect("every broker runs");
        assert!(p.try_wait().unwrap().is_none(), "broker {} exited", i + 1);
    }
}

/// Sequential one-record acks=all produces to one partition's leader, one at a time, timed: the latency a producer
/// that waits for each answer sees. A measurement, not a check (it depends on the disk), so it runs only when asked:
/// `KAFKA3_LATENCY=1 cargo test -p blossom-cli --test it acks_all_latency -- --nocapture`, and `KAFKA3_TAIL=crc` for
/// the WAL's one-sync certification. Otherwise it reports itself skipped.
#[test]
#[allow(clippy::print_stderr)] // The measurement is this test's output.
fn acks_all_latency() {
    if std::env::var_os("KAFKA3_LATENCY").is_none() {
        skipped("acks_all_latency is a measurement: set KAFKA3_LATENCY=1 to run it");
        return;
    }
    let _one = crate::one_cluster();
    let tail = std::env::var("KAFKA3_TAIL").unwrap_or_else(|_| "strict".to_owned());
    let cluster = Cluster::with_storage("latency", &format!("tail_certification = \"{tail}\"\n"));
    let ports = cluster.ports.clone();
    create_topic(&ports);
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(30));
    let leader = loop {
        assert!(clock.elapsed() < limit, "no leader for partition 0");
        if let Some((_, l)) = metadata(&ports, ports[0]) {
            break l[&0];
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut s = connect(leader).unwrap();
    let mut took: Vec<Duration> = Vec::new();
    for n in 0..300 {
        let req = ProduceRequest::default()
            .with_acks(-1)
            .with_timeout_ms(3000)
            .with_topic_data(vec![
                TopicProduceData::default()
                    .with_name(topic_name())
                    .with_partition_data(vec![
                        PartitionProduceData::default()
                            .with_index(0)
                            .with_records(Some(Bytes::from(batch(&[format!("v{n}")])))),
                    ]),
            ]);
        let clock = Stopwatch::start();
        let mut body = call(&mut s, &framed(0, 12, n, &req)).unwrap();
        let d = clock.elapsed();
        ResponseHeader::decode(&mut body, ProduceResponse::header_version(12)).unwrap();
        let r = ProduceResponse::decode(&mut body, 12).unwrap();
        let code = r.responses[0].partition_responses[0].error_code;
        assert_eq!(code, 0, "produce {n} answered {code}");
        // The first few warm the connection and the leader's caches.
        if n >= 20 {
            took.push(d);
        }
    }
    took.sort();
    let at = |q: f64| took[((took.len() - 1) as f64 * q) as usize].as_secs_f64() * 1000.0;
    eprintln!(
        "acks=all latency ({tail}, {} produces): p50 {:.1} ms, p90 {:.1} ms, p99 {:.1} ms, max {:.1} ms",
        took.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    );
}

/// Reports a test skipped because its tool is missing (as `kafka_gate.rs` does).
#[cfg(test)]
#[allow(clippy::print_stdout)] // The notice is this function's purpose: a silent skip would look like a pass.
fn skipped(why: &str) {
    println!("SKIPPED: {why}");
}

#[cfg(test)]
fn on_path(tool: &str) -> Option<PathBuf> {
    let out = Command::new("which").arg(tool).output().ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Kafka's `bin` directory: `$KAFKA_HOME/bin`, or the repository's `.tools/kafka_*/bin`.
#[cfg(test)]
fn kafka_bin() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("KAFKA_HOME") {
        let bin = PathBuf::from(home).join("bin");
        if bin.join("kafka-topics.sh").exists() {
            return Some(bin);
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for base in [repo.clone(), repo.join("../..")] {
        if let Ok(entries) = std::fs::read_dir(base.join(".tools")) {
            for e in entries.flatten() {
                let bin = e.path().join("bin");
                if e.file_name().to_string_lossy().starts_with("kafka_") && bin.join("kafka-topics.sh").exists() {
                    return Some(bin);
                }
            }
        }
    }
    None
}

/// Runs a tool and returns its standard output (and error, for the failure message), failing the test if it fails.
#[cfg(test)]
fn run_tool(cmd: &mut Command, what: &str) -> String {
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{what} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Runs a tool with `lines` on its standard input.
#[cfg(test)]
fn run_with_input(cmd: &mut Command, lines: &[String], what: &str) -> String {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for l in lines {
            writeln!(stdin, "{l}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{what} failed:\n{text}");
    text
}

/// Each partition's leader and replicas, from `kafka-topics.sh --describe`'s lines.
#[cfg(test)]
fn described(text: &str) -> BTreeMap<i32, (i32, Vec<i32>, Vec<i32>)> {
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| l.contains("Partition: ")) {
        let field = |name: &str| {
            line.split('\t')
                .find_map(|f| f.trim().strip_prefix(name).map(|v| v.trim().to_owned()))
                .unwrap_or_default()
        };
        let ids = |v: String| {
            v.split(',')
                .filter(|x| !x.is_empty())
                .map(|x| x.parse::<i32>().unwrap())
                .collect::<Vec<_>>()
        };
        let p: i32 = field("Partition:").parse().unwrap();
        let leader: i32 = field("Leader:").parse().unwrap_or(-1);
        out.insert(p, (leader, ids(field("Replicas:")), ids(field("Isr:"))));
    }
    out
}

/// Stock clients on the three-broker cluster: kcat lists it; `kafka-topics.sh` creates a topic replicated three
/// times; the Java console producer writes to it; with a partition's leader killed, the console consumer still reads
/// every message from the others; `kafka-reassign-partitions.sh` moves the partitions to two replicas each and
/// verifies it; franz-go produces and consumes on a topic replicated three times.
#[test]
fn stock_clients_use_the_replicated_cluster_across_a_broker_failure() {
    let _one = crate::one_cluster();
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    let mut cluster = Cluster::new("tools");
    let all: Vec<String> = cluster.ports.iter().map(|p| format!("127.0.0.1:{p}")).collect();
    let bootstrap = all.join(",");
    if let Some(kcat) = on_path("kcat") {
        let out = run_tool(Command::new(&kcat).args(["-L", "-b", &all[0], "-m", "10"]), "kcat -L");
        assert!(out.contains("3 brokers:"), "{out}");
        for (i, a) in all.iter().enumerate() {
            assert!(out.contains(&format!("broker {} at {a}", i + 1)), "{out}");
        }
    } else {
        skipped("kcat is not installed");
    }
    let topics = |args: &[&str], at: &str| {
        run_tool(
            Command::new(bin.join("kafka-topics.sh"))
                .args(["--bootstrap-server", at])
                .args(args),
            &format!("kafka-topics.sh {args:?}"),
        )
    };
    let created = topics(
        &[
            "--create",
            "--topic",
            "orders",
            "--partitions",
            "3",
            "--replication-factor",
            "3",
        ],
        &bootstrap,
    );
    assert!(created.contains("Created topic orders."), "{created}");
    // Every partition on all three, with a leader, all in sync (once the leaders' first announcements are in).
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(30));
    let parts = loop {
        let d = described(&topics(&["--describe", "--topic", "orders"], &bootstrap));
        if d.len() == 3 && d.values().all(|(l, rs, isr)| *l > 0 && rs.len() == 3 && isr.len() == 3) {
            break d;
        }
        assert!(clock.elapsed() < limit, "the partitions did not settle: {d:?}");
        std::thread::sleep(Duration::from_millis(300));
    };
    let messages: Vec<String> = (0..30).map(|i| format!("order {i}")).collect();
    // The console producer is idempotent by default; acks=all.
    let out = run_with_input(
        Command::new(bin.join("kafka-console-producer.sh")).args([
            "--bootstrap-server",
            &bootstrap,
            "--topic",
            "orders",
            "--producer-property",
            "acks=all",
        ]),
        &messages,
        "the console producer",
    );
    assert!(!out.contains("ERROR"), "{out}");
    // Partition 0's leader goes down for good (kill -9): the others elect a leader and serve every message.
    let victim = (parts[&0].0 - 1) as usize;
    cluster.kill(victim);
    let live: Vec<String> = all
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != victim)
        .map(|(_, a)| a.clone())
        .collect();
    let live = live.join(",");
    let mut read = Vec::new();
    for p in ["0", "1", "2"] {
        let out = run_tool(
            Command::new(bin.join("kafka-console-consumer.sh")).args([
                "--bootstrap-server",
                &live,
                "--topic",
                "orders",
                "--partition",
                p,
                "--offset",
                "earliest",
                "--timeout-ms",
                "15000",
            ]),
            "the console consumer",
        );
        read.extend(out.lines().map(str::to_owned));
    }
    read.sort();
    let mut want = messages.clone();
    want.sort();
    assert_eq!(read, want, "the messages read back with broker {} down", victim + 1);
    cluster.start(victim, false);

    // Each partition moved to two replicas, verified done.
    let plan = dir_file(
        &cluster,
        "plan.json",
        &format!(
            "{{\"version\":1,\"partitions\":[{}]}}",
            (0..3)
                .map(|p| format!(
                    "{{\"topic\":\"orders\",\"partition\":{p},\"replicas\":[{},{}]}}",
                    (p + 1) % 3 + 1,
                    (p + 2) % 3 + 1
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    let reassign = |args: &[&str]| {
        run_tool(
            Command::new(bin.join("kafka-reassign-partitions.sh"))
                .args(["--bootstrap-server", &bootstrap, "--reassignment-json-file"])
                .arg(&plan)
                .args(args),
            &format!("kafka-reassign-partitions.sh {args:?}"),
        )
    };
    let out = reassign(&["--execute"]);
    assert!(out.contains("Successfully started partition reassignment"), "{out}");
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(60));
    loop {
        let out = reassign(&["--verify", "--preserve-throttles"]);
        if (0..3).all(|p| out.contains(&format!("Reassignment of partition orders-{p} is completed"))) {
            break;
        }
        assert!(clock.elapsed() < limit, "the reassignment did not complete: {out}");
        std::thread::sleep(Duration::from_millis(500));
    }
    let (clock, limit) = (Stopwatch::start(), Duration::from_secs(30));
    loop {
        let d = described(&topics(&["--describe", "--topic", "orders"], &bootstrap));
        let moved = (0..3).all(|p| {
            d.get(&p).is_some_and(|(_, rs, _)| {
                rs.len() == 2 && rs.contains(&((p + 1) % 3 + 1)) && rs.contains(&((p + 2) % 3 + 1))
            })
        });
        if moved {
            break;
        }
        assert!(clock.elapsed() < limit, "the replicas did not move: {d:?}");
        std::thread::sleep(Duration::from_millis(300));
    }
    // And the data moved with them.
    let mut read = Vec::new();
    for p in ["0", "1", "2"] {
        let out = run_tool(
            Command::new(bin.join("kafka-console-consumer.sh")).args([
                "--bootstrap-server",
                &bootstrap,
                "--topic",
                "orders",
                "--partition",
                p,
                "--offset",
                "earliest",
                "--timeout-ms",
                "15000",
            ]),
            "the console consumer",
        );
        read.extend(out.lines().map(str::to_owned));
    }
    read.sort();
    assert_eq!(read, want, "the messages read back after the reassignment");

    if let Some(go) = on_path("go") {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/kafka/franz");
        let out = run_tool(
            Command::new(go)
                .args(["run", ".", "produce-consume", &bootstrap, "3"])
                .current_dir(&dir),
            "franz-go",
        );
        assert!(out.contains("ok 300 records"), "{out}");
    } else {
        skipped("Go is not installed");
    }
}

/// Writes `content` to a file in the cluster's directory and returns its path.
#[cfg(test)]
fn dir_file(cluster: &Cluster, name: &str, content: &str) -> PathBuf {
    let path = cluster.dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Java's `String.hashCode`, over UTF-16 code units.
#[cfg(test)]
fn java_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(i32::from(u)))
}

/// Each group's committed offset per partition of `topic`, from `kafka-consumer-groups.sh --describe`'s lines.
#[cfg(test)]
fn group_offsets(text: &str, topic: &str) -> BTreeMap<i32, i64> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 4 && f[1] == topic).then(|| (f[2].parse().unwrap(), f[3].parse().unwrap_or(-1)))
        })
        .collect()
}

/// Consumer groups with stock clients (S9): the Java console consumer in a group reads every message and commits;
/// `kafka-consumer-groups.sh` lists and describes the group with its offsets; with the group's coordinator killed
/// (kill -9), the group's next consumer reads only the messages after the committed offsets, from the new
/// coordinator; kcat (librdkafka, the legacy versions) consumes in a group and resumes from its commits; franz-go's
/// group consumer, and a Java program with two KafkaConsumers, share a topic between two members and commit.
#[test]
fn consumer_groups_with_stock_clients_across_a_coordinator_failure() {
    let _one = crate::one_cluster();
    let Some(bin) = kafka_bin() else {
        skipped("no Kafka distribution (set KAFKA_HOME, or unpack one into .tools/)");
        return;
    };
    // Five offsets partitions (Kafka's default is 50; each is a replication group, and a debug build pays for each).
    let mut cluster = Cluster::with_config("groups", "OFFSETS_PARTITIONS = 5", "");
    let all: Vec<String> = cluster.ports.iter().map(|p| format!("127.0.0.1:{p}")).collect();
    let bootstrap = all.join(",");
    let topics = |args: &[&str], at: &str| {
        run_tool(
            Command::new(bin.join("kafka-topics.sh"))
                .args(["--bootstrap-server", at])
                .args(args),
            &format!("kafka-topics.sh {args:?}"),
        )
    };
    let created = topics(
        &[
            "--create",
            "--topic",
            "events",
            "--partitions",
            "3",
            "--replication-factor",
            "3",
        ],
        &bootstrap,
    );
    assert!(created.contains("Created topic events."), "{created}");
    let produce = |msgs: &[String], at: &str| {
        let out = run_with_input(
            Command::new(bin.join("kafka-console-producer.sh")).args([
                "--bootstrap-server",
                at,
                "--topic",
                "events",
                "--producer-property",
                "acks=all",
            ]),
            msgs,
            "the console producer",
        );
        assert!(!out.contains("ERROR"), "{out}");
    };
    let first: Vec<String> = (0..30).map(|i| format!("event {i}")).collect();
    produce(&first, &bootstrap);
    let consume = |group: &str, at: &str, n: usize, from_start: bool| -> Vec<String> {
        let mut cmd = Command::new(bin.join("kafka-console-consumer.sh"));
        cmd.args(["--bootstrap-server", at, "--topic", "events", "--group", group])
            .args(["--max-messages", &n.to_string(), "--timeout-ms", "60000"]);
        // A partition the group never committed starts at its beginning (the Java consumer commits only partitions it
        // read records from; `latest`, the default, would skip what is already there).
        if from_start {
            cmd.arg("--from-beginning");
        } else {
            cmd.args(["--consumer-property", "auto.offset.reset=earliest"]);
        }
        let mut got: Vec<String> = run_tool(&mut cmd, "the console consumer in a group")
            .lines()
            .map(str::to_owned)
            .collect();
        got.sort();
        got
    };
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    assert_eq!(
        consume("cg", &bootstrap, 30, true),
        sorted(&first),
        "the group read the topic"
    );

    // The group, its offsets committed (each partition at its log end).
    let groups = |args: &[&str], at: &str| {
        run_tool(
            Command::new(bin.join("kafka-consumer-groups.sh"))
                .args(["--bootstrap-server", at])
                .args(args),
            &format!("kafka-consumer-groups.sh {args:?}"),
        )
    };
    let listed = groups(&["--list"], &bootstrap);
    assert!(listed.lines().any(|l| l.trim() == "cg"), "{listed}");
    let offsets = group_offsets(&groups(&["--describe", "--group", "cg"], &bootstrap), "events");
    assert_eq!(offsets.values().sum::<i64>(), 30, "the committed offsets: {offsets:?}");

    // The coordinator: the leader of the group's __consumer_offsets partition. Killed, the group moves.
    let part = (if java_hash("cg") == i32::MIN {
        0
    } else {
        java_hash("cg").abs()
    }) % 5;
    let d = described(&topics(&["--describe", "--topic", "__consumer_offsets"], &bootstrap));
    let victim = (d[&part].0 - 1) as usize;
    cluster.kill(victim);
    let live: Vec<String> = all
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != victim)
        .map(|(_, a)| a.clone())
        .collect();
    let live = live.join(",");
    let second: Vec<String> = (30..40).map(|i| format!("event {i}")).collect();
    produce(&second, &live);
    assert_eq!(
        consume("cg", &live, 10, false),
        sorted(&second),
        "after its coordinator (broker {}) died, the group resumed from its committed offsets",
        victim + 1
    );
    cluster.start(victim, false);

    let mut every = first.clone();
    every.extend(second.iter().cloned());
    if let Some(kcat) = on_path("kcat") {
        let kcat_group = |from_start: bool| -> Vec<String> {
            let mut cmd = Command::new(&kcat);
            cmd.args(["-b", &bootstrap, "-q", "-e", "-X", "session.timeout.ms=6000"]);
            if from_start {
                cmd.args(["-o", "beginning"]);
            } else {
                cmd.args(["-X", "auto.offset.reset=earliest"]);
            }
            cmd.args(["-G", "kg", "events"]);
            let mut got: Vec<String> = run_tool(&mut cmd, "kcat -G").lines().map(str::to_owned).collect();
            got.sort();
            got
        };
        assert_eq!(kcat_group(true), sorted(&every), "kcat read the topic in a group");
        let third: Vec<String> = (40..45).map(|i| format!("event {i}")).collect();
        produce(&third, &bootstrap);
        assert_eq!(
            kcat_group(false),
            sorted(&third),
            "kcat resumed from its group's committed offsets"
        );
        let listed = groups(&["--list"], &bootstrap);
        assert!(listed.lines().any(|l| l.trim() == "kg"), "{listed}");
    } else {
        skipped("kcat is not installed");
    }

    if let Some(go) = on_path("go") {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/kafka/franz");
        let out = run_tool(
            Command::new(go)
                .args(["run", ".", "group", &bootstrap])
                .current_dir(&dir),
            "franz-go group",
        );
        assert!(out.contains("ok group 200 then 20 records"), "{out}");
    } else {
        skipped("Go is not installed");
    }

    // A Java program with Kafka's own client: two KafkaConsumers share the group, committing synchronously.
    let libs = bin.join("../libs");
    let program =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/kafka/java/GroupConsumers.java");
    let out = run_tool(
        Command::new("java")
            .arg("-cp")
            .arg(format!("{}/*", libs.display()))
            .arg(&program)
            .arg(&bootstrap),
        "the Java group program",
    );
    assert!(out.contains("ok java group 300 then 20 records"), "{out}");
}
