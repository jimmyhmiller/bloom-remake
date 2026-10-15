# Kafka gap measurement

Controls use the existing soak workload, frozen blossom-fused-maintenance binary,
and Kafka distribution, with ABBA system order. They add read-only process and
device sampling. Correctness verification and the crash phase remain enabled.
Each runner refuses to overwrite reports and deletes only its own verified scratch.

sample-processes.py records cumulative counters, timestamps, process identities,
and sample overhead. summarize-processes.py compares samples nearest max-phase
boundaries and reports boundary errors. CPU and attributed block writes are
normalized by acknowledged max-phase records across all three brokers. Thread CPU
uses schedstat execution time. Runnable waiting is unavailable with schedstats off.
Sampled states/wchan are not blocked durations. Threads born/dead between endpoints
may be absent from thread deltas; process totals cover them. Bloom stats snapshots
are refreshed once per second and may lag their sampling timestamps.

Diagnostics are separate from controls. The Bloom diagnostic binary uses release
optimization, line tables and frame pointers. A 30-second window starts 15 seconds
into max load. It collects hardware counters, Bloom user-cycle stacks, and BPF
fsync/off-CPU aggregates for both systems. Diagnostic throughput is not a control.
BPF reports completed intervals: waits crossing window boundaries may be absent.
Off-CPU time includes scheduling delay; wakeup/preemption-to-run delay is separately
reported. Aggregated sleeping threads are not a wall-time budget. Linux's reported
preempted TASK_RUNNING state (256) is treated as runnable.

No global kernel tunables, durability policy, producer configuration or language
semantics change. Times from different threads overlap. Inclusive stack costs
must not be summed.

Additional tools:

- `run-trace.sh` / `record-b2.sh`: record only broker 2, verify, retain trace.
- `measure-meta.sh`: identify and time the actual MetaStore::write symbol.
- `cargo run -p blossom-trace --example timeline -- TRACE`: stream tick timestamps
  without printing trace payloads. `replay-windows.py` selects five-second windows
  and runs checked measured/plain replays (override binary with REPLAY_BINARY).
- `summarize-replays.py`: preserve boundary counts, functions, rule/store work and
  preparation/engine/other timing. Store probes include nested API entries.
- `summarize-stacks.py`: user-cycle period-weighted exclusive and overlapping
  inclusive partitions. Parse `perf script --no-inline`; the categorization and
  priority order are explicit in the script.
- `summarize-latencies.py`: scalar sync/off-CPU aggregates; raw histograms retained.
- `run-write-path.sh` / `write-path.bt`: successful VFS writes by basename and
  uninterruptible engine kernel stacks; `summarize-writes.py` groups them. These
  are logical file bytes, exclude mmap writes, and do not equal device bytes.

Runners assume the pre-existing remote Kafka benchmark checkout/distribution and
frozen binaries described in KAFKA-GAP-ANALYSIS.md. Run controls separately from
profiles/builds. Each diagnostic uses a fresh full workload with crash verification.
Generated replay reports include function profiling even in --no-measurement mode;
that switch disables store-operation accounting, not all measurement overhead.
The preparation/engine/other timers are opt-in offline measurements and never
supply semantic time to the executor. The historical blob-map clone remains an
open offline harness issue; timings now expose its surrounding overhead.
