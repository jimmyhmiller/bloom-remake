//! S24: tiered tables (docs/design/DATABASE.md §7). The engine reads a node's durable tables from its database and
//! keeps in memory only what the database does not hold yet. Against the oracle at every tick, through probes on the
//! tables' own keys and on indexes the database builds, ranges, a view rebuilt after a restart, a guarded table and
//! blobs, flushes and compactions and restarts; and a table many times larger than what the engine holds of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;
use blossom_node::durable::DurableSchema;
use blossom_node::manual::ManualDriver;
use blossom_node::recovery::{self, StoreSpec};
use blossom_node::{Backend, Executor, Executors, Node, NodeConfig};
use blossom_store::{OpenMode, SimFs, StoreIdentity, Vfs, WriteFate};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId};
use blossom_value::value::IntValue;

#[cfg(test)]
fn identity() -> StoreIdentity {
    StoreIdentity {
        store_uuid: [0; 16],
        deployment_id: [1; 16],
        program_id: [2; 16],
        node_name: "n1".into(),
        principal: "spiffe://test/tiered/n1".into(),
        format: recovery::FORMAT,
        directory_digest: [3; 16],
    }
}

#[cfg(test)]
/// A deterministic generator (SplitMix64).
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n
    }
}

#[cfg(test)]
struct Fixture {
    artifact: BlsArtifact,
    executors: Executors,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    dir: PathBuf,
}

#[cfg(test)]
impl Fixture {
    fn with_hot_rows(backend: Backend, hot_rows: Option<usize>) -> Fixture {
        Fixture::with(backend, hot_rows, true)
    }

    fn with(backend: Backend, hot_rows: Option<usize>, tiered: bool) -> Fixture {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/db/tiered.bls");
        let (result, _) = compile_file(
            path.to_str().unwrap(),
            &[NodeSpec {
                name: "n1".into(),
                role: None,
            }],
        );
        let artifact = result.unwrap_or_else(|e| panic!("tiered.bls: {e:?}")).0;
        let names: Arc<[Arc<str>]> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        let mut executors = Executors::new(
            backend,
            artifact.program.clone(),
            artifact.roles.clone(),
            names.to_vec(),
            blossom_value::Seed([9; 16]),
            Arc::new(blossom_value::ExternRegistry::new()),
        )
        .unwrap();
        if let Some(rows) = hot_rows {
            executors = executors.with_hot_rows(rows);
        }
        executors = executors.tiered(tiered);
        Fixture {
            schema: DurableSchema::of(artifact.program.get()),
            artifact,
            executors,
            names,
            dir: PathBuf::from("/data/n1"),
        }
    }

    fn boot<'a>(&'a self, fs: &SimFs) -> ManualDriver<'a, Box<dyn Executor>> {
        let fs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let opened = recovery::open(
            fs,
            &StoreSpec {
                dir: self.dir.clone(),
                identity: identity(),
                mode: OpenMode::InitFresh,
                certification: blossom_store::Certification::Strict,
                // Small, so the run flushes on its own and compacts.
                database: blossom_store::lsm::LsmOptions {
                    memtable_bytes: 4 << 10,
                    block_bytes: 512,
                    tier: 3,
                    max_tables: 6,
                    ..blossom_store::lsm::LsmOptions::default()
                },
            },
            &self.artifact.program,
            self.names.clone(),
            Instant(0),
            7,
        )
        .unwrap();
        let exec = self.executors.make(NodeId(0)).unwrap();
        let node = Node::boot(
            NodeConfig::new(NodeId(0), None),
            &self.artifact.program,
            exec,
            opened.boot.clone(),
        )
        .unwrap();
        ManualDriver::new(
            node,
            self.artifact.program.get(),
            &self.schema,
            self.names.clone(),
            opened,
        )
    }

    fn offer(&self, d: &mut ManualDriver<'_, Box<dyn Executor>>, input: &str, values: Vec<Value>) {
        let rel = self.artifact.rel_named(input).unwrap();
        d.node.offer_input(rel, Arc::from(values));
    }

    /// Every durable relation's released rows, by name.
    fn released(&self, d: &ManualDriver<'_, Box<dyn Executor>>) -> BTreeMap<String, Vec<Vec<Value>>> {
        let p = self.artifact.program.get();
        let mut out = BTreeMap::new();
        for (rel, rows) in d.released_image().unwrap().rows {
            let mut rows: Vec<Vec<Value>> = rows.iter().map(|r| r.to_vec()).collect();
            rows.sort();
            out.insert(p.rels.get(rel).unwrap().name.to_string(), rows);
        }
        out
    }
}

#[cfg(test)]
fn u(x: u64) -> Value {
    Value::Int(IntValue::U64(x))
}

/// A clean crash of a copy of `fs` (everything written survives).
#[cfg(test)]
fn crashed(fs: &SimFs) -> SimFs {
    let mut image = fs.fork().unwrap();
    image.crash(&mut |_| WriteFate::Survive).unwrap();
    image
}

#[test]
fn tiered_tables_agree_with_the_oracle_through_restarts_flushes_and_compactions() {
    // With the default hot tier; one so small it is trimmed all the time; and one whose probes keep at most 10 rows,
    // so the whole table is a large prefix whose small ranges are kept.
    for (seed, hot_rows) in [
        (0u64, None),
        (1, Some(8)),
        (2, None),
        (3, Some(8)),
        (4, Some(640)),
        (5, Some(640)),
    ] {
        let k = Fixture::with_hot_rows(Backend::Checked, hot_rows);
        let mut rng = Rng(seed);
        let mut fs = SimFs::default();
        let mut t = 10i64;
        let mut before: Option<BTreeMap<String, Vec<Vec<Value>>>> = None;
        for run in 0..4 {
            let mut d = k.boot(&fs);
            d.run_until_quiescent(Instant(t)).unwrap();
            // A restart recovers exactly the rows released before it.
            if let Some(b) = before.take() {
                assert_eq!(k.released(&d), b, "seed {seed}: run {run} recovered other rows");
            }
            for step in 0..80 {
                let n = 1 + rng.below(4);
                // One change per key a tick (two upserts of a key in a tick are a program error).
                let mut keys = std::collections::BTreeSet::new();
                for _ in 0..n {
                    let key = rng.below(60);
                    if !keys.insert(key) {
                        continue;
                    }
                    match rng.below(17) {
                        // Few keys: a lattice row merged past often.
                        16 => k.offer(&mut d, "bump", vec![u(key % 6), u(rng.below(50))]),
                        15 => k.offer(&mut d, "pend", vec![u(key)]),
                        14 => k.offer(&mut d, "check_val", vec![u(rng.below(8))]),
                        13 => k.offer(&mut d, "raise", vec![u(rng.below(2000))]),
                        12 => k.offer(&mut d, "check_empty", vec![u(key)]),
                        10 => k.offer(&mut d, "check", vec![u(key)]),
                        11 => k.offer(&mut d, "check_value", vec![u(rng.below(2000))]),
                        0..=1 => k.offer(&mut d, "put", vec![u(key), u(rng.below(2000))]),
                        // Few values: many keys share one (a projection's supports above one).
                        2..=3 => k.offer(&mut d, "put", vec![u(key), u(rng.below(8))]),
                        // Drops often enough that the table empties now and then.
                        4 => {
                            for k2 in 0..60 {
                                if rng.below(3) == 0 {
                                    k.offer(&mut d, "drop", vec![u(k2)]);
                                }
                            }
                        }
                        5 => k.offer(&mut d, "find", vec![u(rng.below(2000))]),
                        6 => k.offer(&mut d, "below", vec![u(rng.below(12))]),
                        7 => k.offer(&mut d, "under", vec![u(rng.below(2000))]),
                        8 => {
                            let input = if rng.below(2) == 0 { "arm" } else { "disarm" };
                            k.offer(&mut d, input, vec![u(key)]);
                        }
                        _ => k.offer(
                            &mut d,
                            "stash",
                            vec![u(key), Value::Bytes(Arc::from(format!("b{}", rng.below(5)).as_bytes()))],
                        ),
                    }
                }
                t += 1;
                if let Err(e) = d.run_until_quiescent(Instant(t)) {
                    panic!("seed {seed} (hot rows {hot_rows:?}), run {run}, step {step}: {e}");
                }
                if rng.below(8) == 0 {
                    d.flush().unwrap();
                }
            }
            before = Some(k.released(&d));
            fs = crashed(&fs);
            // Past the time the incarnation reserved.
            t += 10_000_000_000;
        }
    }
}

#[test]
fn a_tiered_table_is_held_on_disk_not_in_memory() {
    // A hot tier of 256 rows: the engine holds that much of the table and its views at most, besides the changes not
    // yet in the database.
    let k = Fixture::with_hot_rows(Backend::Engine, Some(256));
    let fs = SimFs::default();
    let mut d = k.boot(&fs);
    let mut t = 10i64;
    d.run_until_quiescent(Instant(t)).unwrap();
    // 5000 rows, a hundred per tick.
    for batch in 0..50u64 {
        for i in 0..100u64 {
            let key = batch * 100 + i;
            k.offer(&mut d, "put", vec![u(key), u(key % 500)]);
        }
        t += 1;
        d.run_until_quiescent(Instant(t)).unwrap();
    }
    d.flush().unwrap();
    t += 1;
    d.run_until_quiescent(Instant(t)).unwrap();
    let held = d.node.resident_rows().unwrap();
    let names = |d: &ManualDriver<'_, Box<dyn Executor>>| -> Vec<(String, &'static str, usize)> {
        let p = k.artifact.program.get();
        d.node
            .resident_by_store()
            .unwrap()
            .into_iter()
            .map(|(r, kind, n)| (p.rels.get(r).unwrap().name.to_string(), kind, n))
            .collect()
    };
    // The hot tier (256 rows), and the last tick's changes to the table and the views over it (three copy it).
    assert!(
        held < 1000,
        "the engine holds {held} rows of a 5000-row table: {:?}",
        names(&d)
    );
    // Probes still answer: by the value (an index) and by a range of keys.
    k.offer(&mut d, "find", vec![u(7)]);
    k.offer(&mut d, "below", vec![u(30)]);
    t += 1;
    d.run_until_quiescent(Instant(t)).unwrap();
    let released = k.released(&d);
    assert_eq!(
        released["found"].len(),
        10,
        "the rows with value 7: keys 7, 507, ..., 4507"
    );
    assert_eq!(released["swept"].len(), 30);
    // After a restart, once its first tick has rebuilt the views, the engine again holds little.
    drop(d);
    let fs = crashed(&fs);
    let mut d = k.boot(&fs);
    t += 10_000_000_000;
    for _ in 0..3 {
        t += 1;
        d.run_until_quiescent(Instant(t)).unwrap();
    }
    let held = d.node.resident_rows().unwrap();
    assert!(
        held < 500,
        "after a restart the engine holds {held} rows of a 5000-row table"
    );
    assert_eq!(k.released(&d)["kv"].len(), 5000);
}

#[test]
fn storage_tiered_false_keeps_every_table_in_memory() {
    // The deployment's opt-out (`storage.tiered = false`): the engine starts on the database but loads its tables,
    // and agrees with the oracle through a restart.
    for backend in [Backend::Engine, Backend::Checked] {
        let k = Fixture::with(backend, None, false);
        let fs = SimFs::default();
        let mut d = k.boot(&fs);
        let mut t = 10i64;
        d.run_until_quiescent(Instant(t)).unwrap();
        for batch in 0..10u64 {
            for i in 0..100u64 {
                k.offer(&mut d, "put", vec![u(batch * 100 + i), u(i)]);
            }
            t += 1;
            d.run_until_quiescent(Instant(t)).unwrap();
        }
        d.flush().unwrap();
        drop(d);
        let fs = crashed(&fs);
        let mut d = k.boot(&fs);
        t += 10_000_000_000;
        for _ in 0..3 {
            k.offer(&mut d, "find", vec![u(7)]);
            t += 1;
            d.run_until_quiescent(Instant(t)).unwrap();
        }
        let held = d.node.resident_rows().unwrap();
        assert!(
            held >= 1000,
            "{backend:?}: the engine holds {held} rows of a 1000-row table it keeps in memory"
        );
        let released = k.released(&d);
        assert_eq!(released["kv"].len(), 1000);
        assert_eq!(released["found"].len(), 10);
    }
}

/// A restart catches the durable views up from the rows they were computed from (DATABASE.md §8): the database's
/// views at its flushed tick saw what that tick wrote (a guarded table's rows emitted then, a lattice row merged past
/// its carried one), which the tables before it do not hold. Restarted at that tick and after more ticks.
#[test]
fn a_restart_catches_the_views_up_from_what_the_flushed_tick_wrote() {
    for later in [0, 2] {
        let k = Fixture::with_hot_rows(Backend::Checked, None);
        let fs = SimFs::default();
        let t = std::cell::Cell::new(10i64);
        let mut d = k.boot(&fs);
        let tick = |d: &mut ManualDriver<'_, Box<dyn Executor>>, inputs: &[(&str, Vec<Value>)]| {
            for (input, values) in inputs {
                k.offer(d, input, values.clone());
            }
            t.set(t.get() + 1);
            d.run_until_quiescent(Instant(t.get()))
                .unwrap_or_else(|e| panic!("{later} ticks after the flush: {e}"));
        };
        tick(
            &mut d,
            &[
                ("bump", vec![u(1), u(3)]),
                ("arm", vec![u(2)]),
                ("put", vec![u(1), u(50)]),
                ("put", vec![u(2), u(150)]),
                ("raise", vec![u(100)]),
            ],
        );
        // The flushed tick: rows emitted into a guarded table, a lattice row merged past, and a `resolve` table's
        // new row (written the tick before, for this one).
        tick(
            &mut d,
            &[
                ("bump", vec![u(1), u(5)]),
                ("arm", vec![u(3)]),
                ("bump", vec![u(4), u(1)]),
                ("raise", vec![u(40)]),
            ],
        );
        d.flush().unwrap();
        for i in 0..later {
            tick(&mut d, &[("bump", vec![u(1), u(7 + i)]), ("disarm", vec![u(3)])]);
        }
        let before = k.released(&d);
        drop(d);
        let fs = crashed(&fs);
        let mut d = k.boot(&fs);
        t.set(t.get() + 10_000_000_000);
        tick(&mut d, &[]);
        assert_eq!(
            k.released(&d),
            before,
            "{later} ticks after the flush: recovered other rows"
        );
        for key in 1..5 {
            tick(&mut d, &[("check", vec![u(key)])]);
        }
        tick(
            &mut d,
            &[
                ("bump", vec![u(1), u(2)]),
                ("disarm", vec![u(2)]),
                ("bump", vec![u(4), u(9)]),
                ("raise", vec![u(200)]),
            ],
        );
        for key in 1..5 {
            tick(&mut d, &[("check", vec![u(key)]), ("put", vec![u(key), u(key * 60)])]);
        }
    }
}

/// A WAL record logs how a tick's rows of the views' sources differ from the carried ones (DATABASE.md §8): as bits
/// over the change's rows where they are among them, and as rows where not; a record without them reads as none.
#[test]
fn a_record_logs_the_rows_a_tick_wrote_beyond_the_carried_ones() {
    let k = Fixture::with_hot_rows(Backend::Engine, None);
    let codec = blossom_node::durable::DurableCodec::new(k.artifact.program.get(), &k.schema, k.names.clone());
    let watched = k.artifact.rel_named("watched").unwrap();
    let armed = k.artifact.rel_named("armed").unwrap();
    let row = |k: u64| -> blossom_oracle::Row { Arc::from(vec![Value::Int(IntValue::U64(k))]) };
    let rows = |ks: &[u64]| ks.iter().map(|k| row(*k)).collect::<Vec<_>>();
    let mut delta = blossom_node::durable::Delta::default();
    delta
        .changes
        .insert(watched, (rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9]), rows(&[20, 21])));
    let plain = codec.decode_delta(&codec.encode_delta(&delta).unwrap()).unwrap();
    assert_eq!(plain, delta, "a record without written rows");
    // Among the inserts (past one byte of bits), not among them (a row written and deleted in the tick), a carried
    // row not kept among the deletes and not; and a relation the tick did not change.
    delta.written.insert(watched, (rows(&[2, 9, 40]), rows(&[21, 50])));
    delta.written.insert(armed, (rows(&[7]), Vec::new()));
    let decoded = codec.decode_delta(&codec.encode_delta(&delta).unwrap()).unwrap();
    assert_eq!(decoded, delta);
}
