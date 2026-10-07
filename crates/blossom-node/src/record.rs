//! Recording a node's inputs (`blossom run --record`, ARCHITECTURE §6.4): an [`Executor`] that writes every tick it
//! runs to a [`blossom_trace::node`] trace before running it, so the incarnation can be replayed exactly and
//! questioned afterwards. The node performs no I/O of its own (ARCH-03): the host hands it the sink (a file it
//! created readable by its owner only, since the trace holds the deployment's seed).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::{Arc, Mutex};

use blossom_base::{FnId, RelId, RuleId};
use blossom_ir::tick::{EvalError, FnWork, Instance, Row, RuleWork, StepInput, StepOutput};
use blossom_trace::node::{NodeRecord, NodeTraceHeader, TraceWriter, outcome_digest};
use blossom_value::{BlobRef, BlobSource};

use crate::eval::Executor;

/// An executor that records its inputs, then runs them on `inner`.
pub struct Recording {
    inner: Box<dyn Executor>,
    out: TraceWriter<Box<dyn Write + Send>>,
    /// Blobs the trace holds, and blobs this incarnation created (a replay creates them again).
    written: BTreeSet<BlobRef>,
    created: BTreeSet<BlobRef>,
}

impl Recording {
    /// Records into `sink` (flushed after every tick), starting with `header`.
    pub fn new(
        sink: Box<dyn Write + Send>,
        header: &NodeTraceHeader,
        inner: Box<dyn Executor>,
    ) -> Result<Recording, EvalError> {
        let out = TraceWriter::new(sink, header).map_err(|e| EvalError::Trace(e.to_string()))?;
        Ok(Recording {
            inner,
            out,
            written: BTreeSet::new(),
            created: BTreeSet::new(),
        })
    }

    fn write(&mut self, r: &NodeRecord) -> Result<(), EvalError> {
        self.out.record(r).map_err(|e| EvalError::Trace(e.to_string()))
    }

    fn flush(&mut self) -> Result<(), EvalError> {
        self.out.flush().map_err(|e| EvalError::Trace(e.to_string()))
    }
}

/// A blob source that notes every blob read from it.
#[derive(Debug)]
struct Tap<'a> {
    inner: &'a dyn BlobSource,
    reads: Mutex<Vec<(BlobRef, Arc<[u8]>)>>,
}

impl BlobSource for Tap<'_> {
    fn get(&self, b: &BlobRef) -> Option<Arc<[u8]>> {
        let bytes = self.inner.get(b)?;
        // A poisoned lock means a reader panicked mid-tick, which already fails the tick; the read is still served.
        if let Ok(mut reads) = self.reads.lock() {
            reads.push((*b, bytes.clone()));
        }
        Some(bytes)
    }
}

impl Executor for Recording {
    fn reset(&mut self, carried: Instance) -> Result<(), EvalError> {
        let image: Vec<(RelId, Vec<Row>)> = carried
            .rels
            .iter()
            .map(|(r, rows)| (*r, rows.iter().cloned().collect()))
            .collect();
        self.write(&NodeRecord::Boot { image })?;
        self.flush()?;
        self.inner.reset(carried)
    }

    fn step(&mut self, input: &StepInput<'_>, observe: &[RelId]) -> Result<StepOutput, EvalError> {
        self.write(&NodeRecord::Tick {
            tick: input.tick,
            now: input.now,
            events: input.events.to_vec(),
            delivered: input.delivered.to_vec(),
            ingress: input.ingress.to_vec(),
        })?;
        // Written before the tick runs: a tick that kills the process is in the trace.
        self.flush()?;
        let tap = Tap {
            inner: input.blobs,
            reads: Mutex::new(Vec::new()),
        };
        let result = self.inner.step(
            &StepInput {
                blobs: &tap,
                ..input.clone()
            },
            observe,
        );
        let reads = tap.reads.into_inner().unwrap_or_default();
        for (blob, bytes) in reads {
            if self.created.contains(&blob) || !self.written.insert(blob) {
                continue;
            }
            self.write(&NodeRecord::Blob {
                blob,
                bytes: bytes.to_vec(),
            })?;
        }
        match &result {
            Ok(out) => {
                self.created.extend(out.blobs.keys().copied());
                let digest = outcome_digest(&out.changes, &out.outbox, &out.egress, &out.host);
                self.write(&NodeRecord::Outcome {
                    tick: input.tick,
                    digest,
                })?;
            }
            Err(e) => self.write(&NodeRecord::Failed {
                tick: input.tick,
                error: e.to_string(),
            })?,
        }
        self.flush()?;
        result
    }

    fn carried_rows(&self, rel: RelId) -> Result<Vec<Row>, EvalError> {
        self.inner.carried_rows(rel)
    }

    fn carried(&self) -> Result<Instance, EvalError> {
        self.inner.carried()
    }

    fn rows_examined(&self) -> Option<u64> {
        self.inner.rows_examined()
    }

    fn work_by_rule(&self) -> Option<BTreeMap<RuleId, RuleWork>> {
        self.inner.work_by_rule()
    }

    fn profile_functions(&mut self, on: bool) -> bool {
        self.inner.profile_functions(on)
    }

    fn work_by_function(&self) -> Option<BTreeMap<FnId, FnWork>> {
        self.inner.work_by_function()
    }

    fn holds_blob(&self, b: &BlobRef) -> bool {
        self.inner.holds_blob(b)
    }
}
