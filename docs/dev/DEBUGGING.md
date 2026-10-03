# Debugging a running Blossom program

A Blossom node's tick is a pure function of what it received, its carried state, the deployment seed and the blob
bytes it read. So a recording of every tick's inputs replays the node exactly, and the replay can answer any
question about what the node held, sent, derived and why — after the fact, from a real cluster run, without
adding logging and running it again.

## Record

```
blossom run --deploy deploy.toml --node b1 --record traces/
```

Each incarnation writes `traces/<node>-<incarnation>.blstrace`: the boot image (what it recovered), then per tick
its inputs (events, delivered messages, client requests), the blobs it read, and a digest of its outcome. The file
holds the deployment's seed, so it is created mode 0600. A node killed mid-tick leaves a torn last record, which
the reader ignores.

The kafka3 tests record their brokers when `BLOSSOM_RECORD_DIR` is set: `$DIR/<tag>/traces/`, with the
`deploy.toml` (and secrets) the traces were recorded under.

## Question

Every subcommand takes the trace and the `--deploy` it was recorded under (its program is compiled, and must be the
one recorded), replays up to what it needs, and stops on a divergence with the tick.

| Command | Answers |
|---|---|
| `blossom trace replay T --deploy D` | Does every tick replay to the recorded outcome? Which ticks ran. |
| `… replay --slow MS` | Which ticks took at least MS to evaluate (in replay) or started MS after the tick before (on the recorded node), what they received, and the rules that did the most join work. |
| `… show --rel R --at N [--match P]` | A relation's rows at tick N: tables, views, events, channels. |
| `… history P` | Every tick rows matching P changed: a table's rows added/removed (`+`/`-`, for the next tick), a channel's messages received (`<-`, with the sender) and sent (`->`), an event's rows (`!`), a view's rows starting and stopping to hold. Each line carries the tick's time, so histories of different nodes line up. |
| `… why --at N P` | The rule firings that derived the rows matching P at tick N, and the rows each read. |
| `… whynot --at N P` | For each rule that could derive a row matching P: how far its body got (the deepest prefix some valuation satisfied) and the first literal none passed. Ask again about that literal's relation to go deeper. |
| `… profile --at N` | Rows examined per rule at tick N: where an expensive tick's time went. |

A pattern is `rel(v, …)`, one value per column of the relation's IR form (a channel's destination is column 0), `_`
for any value; values are written as `show` prints them (`3`, `"s"`, `b"bytes"`, `(a, b)`, `None`, `Some(x)`, a node
by name).

## A worked example: partitions without a stable leader

The kafka3 kill -9 test failed with "the partitions could not all be read" (S9). Black-box, it looked like a Raft
liveness bug. With traces:

1. `show --rel leader` / `won` / `partition_leader` at each broker's last tick: every group had a leader and every
   broker agreed — the cluster had converged, after the test gave up.
2. `history 'won(_, _)'` on each broker: leadership of every group rotated every few seconds, right to the end.
3. `history 'beat(_, _, _, _, _, _, _)'`: beats arrived in bursts 3–4 s late (receive time minus the `sent` stamp
   they carry), at both senders at once — the receiver had stopped.
4. `history 'raft_heartbeat(_, _)'`: 3.5–4.3 s gaps between a broker's ticks, rotating between brokers.
5. `replay --slow 300`: one tick per gap, replaying in the same 3.7 s, having received a single Kafka request (a
   Fetch from offset 0). Evaluation, not I/O.
6. `profile --at N`: `fetch_chain` examined 3.7 million rows to walk 154 batches.

The engine evaluated recursive strata naively (n rounds re-deriving n rows), and planned the chain's second probe
with only a range (a guard that cannot fail is ordered before the fallible `let base = last + 1`). Both fixed in the
engine (semi-naive recursion; a later binding keys a probe); the recorded traces still replay to the same outcomes,
and the Fetch ticks take milliseconds.
