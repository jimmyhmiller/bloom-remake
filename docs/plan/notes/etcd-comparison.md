# The etcd comparison

Step 4 of the flagship goal: "I'll run the same workload against etcd on the same machine, with the same durability
settings and the same client, and report both numbers honestly."

## Setup

- **Machine:** Apple M2 Max, 12 cores, macOS (Darwin arm64), one internal SSD. Every process runs on the one machine.
- **Blossom:** the Raft KV (`examples/e11_raft_kv.bls`) as three `blossom run` processes over TCP on localhost, running
  the S5 engine.
- **etcd:** 3.6.5, three members on localhost, default settings.
- **Client:** `blossom-kv`, the same closed-loop workload for both.
  - Each client has one operation outstanding and draws from 1,000 keys: 50% puts of 16-byte values, 50% gets.
  - For etcd, it talks to the v3 JSON gateway; for Blossom, to the e11 client protocol.
  - Every run records its history, and the WGL checker verifies it is linearizable.
- **Durability:** both sync every commit with `F_FULLFSYNC` before acknowledging it.
  - Blossom: Rust's `sync_data` issues `F_FULLFSYNC` on Apple targets.
  - etcd: its darwin `Fdatasync` issues `F_FULLFSYNC`.
- **Script:** `scripts/bench-raft-vs-etcd.sh` (`ETCD_BIN=… [ETCD_BENCH=…]`) reproduces every number here.

## Blossom's two tail certifications

- **`crc`:** one fsync per group commit; records carry checksums. This is etcd's model. Recovery truncates damage in
  the newest batch as a torn tail, so damage to the last acknowledged batch is not told from a crash.
- **`strict`:** four fsyncs per group commit (the data, a sync marker, and an acknowledgement receipt with its
  directory). This is Blossom's default. Recovery can then always tell corruption of acknowledged records from a torn
  tail.

The same-durability comparison is `crc` against etcd. Both are reported.

## Results (2026-09-29)

Each cell is one 20 s run: throughput, then p50 / p99 latency. For each system, runs at the same concurrency are
separate trials. Every history checked linearizable (17 runs: these 16 and the `strict` one below).

| Clients | Blossom `crc` | etcd (same client) |
|---|---|---|
| 1 | 73 ops/s, 13.1 / 36 ms | 184 ops/s, 2.8 / 26 ms |
| 1 | 73 ops/s, 13.1 / 35 ms | 168 ops/s, 4.7 / 30 ms |
| 16 | 513 ops/s, 29.9 / 60 ms | 320 ops/s, 45.1 / 130 ms |
| 16 | 571 ops/s, 27.6 / 61 ms | 340 ops/s, 42.6 / 121 ms |
| 16 | 457 ops/s, 34.2 / 63 ms | 315 ops/s, 44.7 / 164 ms |
| 64 | 1,703 ops/s, 36.0 / 71 ms | 1,058 ops/s, 54.6 / 134 ms |
| 64 | 1,684 ops/s, 36.1 / 80 ms | 1,089 ops/s, 52.8 / 128 ms |
| 64 | 1,789 ops/s, 33.9 / 62 ms | 1,067 ops/s, 54.7 / 131 ms |

Blossom `strict`, 16 clients: 135 ops/s, 111.9 / 430 ms, about 3.8x below `crc`. The three extra `F_FULLFSYNC`s per
commit are its cost.

etcd's own gRPC `benchmark` (a different client: puts only, native gRPC) got 343 puts/s (47 ms average, 163 ms p99)
at 16 clients, and 1,371 puts/s (47 ms average, 110 ms p99) at 64.

## Reading the numbers

- **One client:** etcd is 2.3–2.5x faster, and 3–5x lower at p50.
  - A put in either system costs a leader fsync and a follower fsync.
  - etcd serves a linearizable get with ReadIndex: a heartbeat round, no disk.
  - e11 puts every get into the Raft log, so a get costs the same fsyncs as a put. This design is correct, but slow
    for reads.
  - Blossom's leader also syncs an entry before sending it; etcd's leader sends before its own fsync finishes.
- **16 and 64 clients:** Blossom `crc` serves about 1.5–1.7x etcd's throughput through the same client, at lower
  p50 and about half the p99.
  - Both are bound by `F_FULLFSYNC` latency, so throughput is how many operations each group commit carries.
  - Blossom's node pipelines ticks while the committer syncs, so a sync covers everything submitted meanwhile.
  - etcd also fsyncs its bbolt backend periodically, which competes for the same disk.
- **The client's cost for etcd:**
  - At 16 clients, the JSON gateway and native gRPC give about the same throughput (≈320 against 343), so the
    gateway is not the limit there.
  - At 64 clients, native gRPC puts reach 1,371/s against 1,058 mixed operations through the gateway. That is still
    below Blossom's 1,684–1,789 mixed operations through its own protocol, but the gap is smaller.
- **What this does not show:** a single machine and a single SSD, with every member competing for one disk.
  - Keys are uniform over 1,000 and values are 16 bytes.
  - There are no failures during these runs; the S4 tests cover behaviour under faults.
  - etcd runs untuned defaults, and so does Blossom.
  - Blossom's default durability (`strict`) is slower than etcd, not faster.
