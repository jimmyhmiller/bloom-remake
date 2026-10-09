# RF: replication under large batches (2026-10-08)

The user asked what "a new replica copies the whole retained history entry by entry" (OBJECT-STORAGE.md §5.1.4)
means, whether it is a good idea, and whether it is a foot gun for a large-scale Kafka test.

## The copy itself

A Kafka partition's Raft log is its partition log: one entry per record batch. A snapshot of a Kafka partition holds
nothing (its state is the records), so the log is compacted only past what retention deleted (`compactable` in
`produce_node.bls`). A new replica therefore gets every retained batch through AppendEntries. That is what Kafka
does (a new follower fetches from its leader's log start), and it is inherent: only tiered storage plus a change to
the Raft program would avoid it. Not a defect.

## Defects found in how it copies

1. **High. A replication message could be too large for a frame, and was dropped for ever.** `RAFT_BATCH` (64) capped
   a message by entries, not bytes; an entry is a batch of up to `max.message.bytes` (default 1 MiB). A row over the
   16 MiB frame (`WireLimits::max_frame`) is counted in `dropped_oversized` and dropped. The leader sent the same
   range again at every heartbeat and the follower never moved. Reproduced (release build): the leader dropped
   1 037 messages and the batch after the catch-up was never acknowledged. The same applied to live replication
   whenever a follower fell 16 MiB behind. `meta_snap` had had the same defect (HD).
   - Fix: a batch carries at most `RAFT_BATCH_BYTES` (4 MiB, a param) of entries, and always the first
     (`batch_end`, `out_sizes`, `out_end`); sizes come from the blobs' lengths, and only the entries kept are read.
     `rsent` advances to where the batch ended (`pushed`), and past a snapshot point at once (`pushed_compacted`).
   - An entry alone must fit a frame: `RAFT_ENTRY_MAX` (15 MiB) in `raft.bls`, checked at append (a hard error:
     an includer that proposes more is a bug). The Kafka side keeps to it with `ENTRY_MAX` (`protocol.bls`):
     `max.message.bytes` above it is INVALID_CONFIG (it accepted up to 2^31 - 1), and an offset commit whose
     entry would exceed it is answered INVALID_COMMIT_OFFSET_SIZE (28) for each partition, as Kafka answers a commit
     too large for the offsets topic.
2. **Medium. A down follower cost its leader a batch read and encoded at every heartbeat.** The heartbeat re-send
   went to every member from its acknowledged index, alive or not: with large batches the leader read up to 64 of
   them 20 times a second for nothing. Now the heartbeat re-send goes only to a follower whose broker is heard alive
   (`peer_alive`); the ack path and new entries are unchanged.

## Found, not fixed

- **A debug build cannot keep up with ~1 MiB batches.** One 900 KiB batch makes ticks of 230–590 ms on the leader;
  followers' acknowledgements wait in its queue past CheckQuorum's period (`RAFT_ELECTION_MIN`, 300 ms), the
  leader steps down, and leadership flaps. The new test therefore runs from a release build (`scripts/test-tiers.sh
  full`; a debug build skips it). CheckQuorum counting acknowledgements by when they are processed, not received,
  makes slow ticks look like a lost quorum: worth a look under load in release too.
- **Catch-up is one batch per round trip,** each with a follower fsync, and at most 64 entries: a long history of
  small batches catches up slowly. Kafka bounds a fetch by bytes only.
- **While a follower catches up, each heartbeat re-sends its in-flight batch** (the ack path already sent it):
  up to `RAFT_BATCH_BYTES` extra per heartbeat per lagging follower.
- **The simulator does not model the frame limit,** so no simulated test could have found defect 1; only real
  processes drop oversized rows.

## Tests

- `kafka3::a_restarted_follower_catches_up_on_batches_near_the_size_limit` (blossom-cli, full tier, release):
  a follower misses 40 batches of ~900 KiB, comes back, then the other follower is killed; the next batch is
  acknowledged only once the returned follower holds everything. Fails before the fix (above); passes after.
  Mutation: `RAFT_BATCH_BYTES` = 1 GiB fails it.
- `kafka_codec::max_message_bytes_is_bounded_by_a_replicated_entry`: `config_value_ok` at the bound, one past it,
  and the old maximum. `kafka_topics`' model takes the same bound (its generator is unchanged: adding configurations
  to it shifts the seeded runs, and two fault checks then found no unanswered request).
- **Not tested:** the INVALID_COMMIT_OFFSET_SIZE answer. A commit over 15 MiB is too heavy for the simulated group
  scenario; the group suites check that ordinary commits are unchanged.
