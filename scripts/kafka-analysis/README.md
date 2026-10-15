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
