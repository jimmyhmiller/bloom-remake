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
  - Both clients talk to the leader. Blossom sessions follow redirects to it; etcd sessions find it through
    `/v3/maintenance/status`. `ETCD_ROUTE=spread` spreads etcd sessions over the three endpoints instead, so
    followers forward. The first version of this comparison did that by accident, which penalized etcd; the S5
    review caught it.
  - Every run records its history, and the WGL checker verifies it is linearizable.
- **Durability:** both sync every commit with `F_FULLFSYNC` before acknowledging it.
  - Blossom: Rust's `sync_data` issues `F_FULLFSYNC` on Apple targets.
  - etcd: its darwin `Fdatasync` issues `F_FULLFSYNC`.
- **Script:** `scripts/bench-raft-vs-etcd.sh` (`ETCD_BIN=… [ETCD_BENCH=…]`) reproduces every number here.

## Blossom's two tail certifications

- **`crc`:** one fsync per group commit, as etcd's WAL does. Records carry checksums, and a batch's sync marker is
  written at the head of the next batch.
  - Recovery refuses damage to any acknowledged batch but the last.
  - Damage confined to the last acknowledged batch is truncated as a torn tail. It can't be told from a torn write
    of the next, unsynced batch, whose sectors may be lost in any order.
  - etcd assumes a write tears only at its end, so it would refuse to start there. crc trades that detection for
    tolerating out-of-order sector loss.
- **`strict`:** four fsyncs per group commit (the data, a sync marker, and an acknowledgement receipt with its
  directory). This is Blossom's default. Recovery can then always tell corruption of acknowledged records from a torn
  tail.

The same-durability comparison is `crc` against etcd. Both are reported.

## Results (2026-09-29, both clients at their leaders)

Each cell is one 20 s run: throughput, then p50 / p99 latency. Rows at the same concurrency are separate trials. Every
history checked linearizable: 15 runs, the 14 here plus the `strict` one below.

| Clients | Blossom `crc` | etcd (same client, at its leader) |
|---|---|---|
| 1 | 78 ops/s, 12.7 / 23 ms | 188 ops/s, 1.7 / 25 ms |
| 1 | 70 ops/s, 12.7 / 45 ms | 190 ops/s, 1.7 / 25 ms |
| 16 | 581 ops/s, 25.8 / 80 ms | 490 ops/s, 30.7 / 66 ms |
| 16 | 620 ops/s, 24.8 / 51 ms | 484 ops/s, 31.1 / 70 ms |
| 16 | 636 ops/s, 24.1 / 50 ms | 429 ops/s, 31.6 / 164 ms |
| 64 | 1,698 ops/s, 34.0 / 143 ms | 1,653 ops/s, 36.0 / 81 ms |
| 64 | 1,765 ops/s, 34.4 / 67 ms | 1,784 ops/s, 33.5 / 74 ms |

Blossom `strict`, 16 clients: 142 ops/s, 110 / 192 ms, about 4.3x below `crc`. The three extra `F_FULLFSYNC`s per commit
are its cost.

etcd's own gRPC `benchmark` (a different client: puts only, native gRPC, spread over the three endpoints) got
363 puts/s at 16 clients (44 ms average, 163 ms p99).

## Reading the numbers

- **One client:** etcd is about 2.5x faster, and about 7x lower at p50 (1.7 ms against 12.7 ms).
  - Half the operations are gets. etcd serves a linearizable get with ReadIndex, a heartbeat round with no disk,
    which on localhost is sub-millisecond.
  - e11 puts every get into the Raft log, so a get costs the same fsyncs as a put (a leader and a follower
    `F_FULLFSYNC`). This design is correct but slow for reads.
  - Blossom's leader also syncs an entry before sending it; etcd's leader sends before its own fsync finishes.
  - Blossom's p50 is about two `F_FULLFSYNC`s and a few ticks.
- **16 clients:** Blossom `crc` serves about 1.2–1.5x etcd's throughput (581–636 against 429–490 ops/s), at lower
  p50. The p99s are comparable, each with one bad run.
- **64 clients:** they are even: Blossom 1,698–1,765 ops/s, etcd 1,653–1,784.
  - Both are bound by `F_FULLFSYNC` latency, so throughput is how many operations each group commit carries.
  - etcd's p99 is steadier in these runs (74–81 ms against 67–143 ms).
- **Routing matters for etcd.** A first version of this comparison spread etcd's sessions over its endpoints, so
  followers forwarded two thirds of the operations. etcd then measured 315–340 ops/s at 16 clients and 1,058–1,089
  at 64; routed to its leader it gets 429–490 and 1,653–1,784. The S5 review caught the asymmetry;
  `ETCD_ROUTE=spread` reproduces it.
- **What this does not show:**
  - One machine and one SSD, with every member competing for one disk.
  - Uniform keys over 1,000 and 16-byte values.
  - No failures during these runs; the S4 tests cover behaviour under faults.
  - Untuned defaults for both.
- **Durability:** Blossom's default (`strict`) is well below etcd. At etcd's level of durability (`crc`), Blossom is
  competitive under concurrency and much slower at one client, because of how e11 reads.
