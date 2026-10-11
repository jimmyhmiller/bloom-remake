# SCALE: the Kafka soak against Apache Kafka (2026-10-11)

The user asked for "a non-trivial scale (not massive, but reasonable) head to head test of our kafka vs real kafka"
on computer.jimmyhmiller.com. Choices (question tool): a sustained soak (10 topics x 12 partitions, RF 3, 4
producers and a 4-consumer group running throughout; phases max rate, fixed rate, catch-up read); a broker killed
with kill -9 in the fixed phase and every acknowledged record verified afterwards; brokers pinned to the same cores;
if Blossom falls over, diagnose with the trace tooling, fix properly, rerun.

## The harness

- `scripts/kafka-soak/Soak.java`: the load client, Kafka's own Java client (kafka-clients 4.0) for every system.
  `produce` (acks=all, idempotent, paced without coordinated omission: latency is measured from each record's
  slot), `consume` (a group member; end-to-end latency from the slot), `verify` (reads every partition from its
  start; each acknowledged record must be at the offset its acknowledgement gave; no duplicates).
- `scripts/bench-kafka-soak.sh SYSTEM`: the phases, the kill -9 of the broker leading most partitions, per-broker
  CPU/RSS and disk sampling, the catch-up read and the verify. Results in one directory per run.
- `scripts/bench-kafka.sh`: per-broker start/kill/restart (`kill N`, `restart N`), `BROKER_CPUS` pinning,
  `PARTITIONS=0`.
- Pinning on the server (Ryzen AI Max+ 395, 16 cores / 32 threads; CPU n and n+16 are siblings): brokers
  `0-3,16-19`, `4-7,20-23`, `8-11,24-27`; clients `12-15,28-31`.

## Smoke runs (20 s max, 60 s fixed at 2000 records/s, kill at 20 s)

- **Kafka 4.0** (fsync per append): max ~29.6k records/s over 120 partitions; fixed-phase ack p50 120-200 ms before
  the kill (fsync per append across 120 partitions); ~10 s of stalled acks at the kill (KRaft session timeout 9 s);
  verify OK: 830 789 acknowledged, none lost, none duplicated.
- **Blossom** fell over: of ~162k records offered in the max phase ~75k were acknowledged (the rest timed out after
  the client's 120 s delivery timeout); the fixed phase acknowledged ~10k of 120k; brokers stayed at 100% CPU after
  the load stopped, RSS grew 0.5 -> 3.4 GB, DescribeGroups timed out. Stopped; evidence in
  `~/soak-evidence/smoke1` on the server.

## Diagnosis

Reproduced with no consumers: 4 soak producers at 2000 records/s in all over the 10 topics, 20 s
(`~/soak-exp2.sh 4 2000 20 0`): ack p50 0.75-1.8 s, p99 up to 115 s, records failing at 120 s, while the brokers
were only 25-30% busy. A single `kafka-producer-perf-test` producer at 4000 records/s over 120 partitions does not
show it (fewer, fuller batches: fewer blobs).

1. The Java client's log (WARN) showed 906 NOT_LEADER_OR_FOLLOWER and 861 OUT_OF_ORDER_SEQUENCE_NUMBER on 95 of the
   120 partitions: leaders kept changing, and an idempotent producer stalls a partition until its delivery timeout
   after an out-of-order sequence.
2. Recorded traces (`RECORD=1`; `blossom trace history … "won(_, _)"` and `"stepped_down(_, _)"`): 153 leaderships
   lost in 20 s, every one a CheckQuorum step-down, in bursts of 6-30 groups at once.
3. For one deposed group, both followers' last acknowledgements were 312 ms before the step-down. In that window
   every broker kept ticking but took no acknowledgement; `beat`/`ack` histories showed whole peer links silent for
   ~300 ms (b2 -> b1: nothing, then 70 beats and 174 acks in one tick).
4. A node releases a tick's messages only once the committer has synced its WAL record. The committer also made the
   WAL's truncation after each database flush: `BlobStore::sync_logged_below`, an `fdatasync` and a rename for every
   blob logged since the previous flush (every Kafka record batch is a blob). New stats (`wal_truncate_*`, then
   `blob_promote_*`, `blobs_promoted`): each truncation took up to **1.06-1.08 s** for ~700 blobs/s per broker
   (~5700 blobs in the run). For that long nothing a broker sent left it, so its peers' leaders heard no
   acknowledgements for a CheckQuorum period and stepped down, all at once.

## Fix 1 (done): blob promotion off the committer

- `blossom-runtime`: a `promoter` thread makes a flush's blobs durable; the committer hands it each truncation
  (`Promotion`) and drops the WAL segments once it reports back (it waits for work at most `TRUNCATE_POLL`, 100 ms,
  so it also takes finished promotions while idle). Shutdown has no channel cycle: the promoter's channel closes
  with the committer.
- `blossom-store::BlobStore`: safe for that concurrency, which the inline call never exercised. A pending blob's
  provisional file is never written again (logged anew, its earlier record outlives the file's sync; rewriting it
  while a promotion was between its sync and its rename could leave unsynced bytes under the blob's name and lose the
  blob once the WAL went), and promotions take turns (`promoting`): a truncation's and a put's promotion of one blob
  never interleave (the loser could otherwise find the file gone while the blob is still pending, a WAL failure).
  Tests: `a_pending_blob_logged_again_keeps_its_file_and_its_first_record`,
  `promotions_run_beside_logging_and_puts` (threads; a crash after; both fail with the old `write_logged`).
- Result: no record times out (40 000 of 40 000 acknowledged; the producers finish in 27 s, not 125+), but WAL syncs
  now take up to **0.56-0.92 s**: ~10 400 blob `fdatasync`s per broker compete with the WAL's on the one disk, so
  leaders still step down (~650 NOT_LEADER / out-of-order errors) and ack p99 is ~7.5 s.

## The cost of a sync per blob (server, ext4 on NVMe, 5000 blobs of 1 KiB)

| How | Time | Per blob |
|---|---|---|
| `fdatasync` + rename each, serially (today) | 1.613 s | 323 us |
| the same on 8 threads | 0.413 s | 83 us |
| the same on 32 threads | 0.193 s | 39 us |
| one file, one `fdatasync` | 0.019 s | ~4 us |

## Also seen (not yet looked at)

- Ack p99 at a fixed 1000 records/s is 1-3 s even with 6 partitions (S10 measured p99 14 ms at 2000 records/s):
  probably the same sync stalls (flushes come every few seconds under load).
- The soak's Blossom smoke run: RSS grew to 3.4 GB under the collapse (queues behind stalled links?).
