//! A synchronous driver (ARCHITECTURE §5.10 `ManualDriver`): runs a node's ticks on the calling thread, appending
//! and syncing each tick's WAL record before running the next. No pipelining: it is the simplest correct driver, for
//! tests, tools and embedding without threads. The network runtime pipelines the same node (`blossom-runtime`).

use blossom_base::internal_error;
use blossom_ir::core::Program;
use blossom_store::{CheckpointWriter, MetaRecord, MetaStore, SyncedTick, WalRecordBuf, WalWriter};
use blossom_value::time::{Instant, Tick};

use crate::durable::DurableCodec;
use crate::node::{Node, ReleasedTick, TickEffects};
use crate::recovery::{Opened, tick_record};
use crate::{Executor, NodeError};

pub struct ManualDriver<'p, E: Executor> {
    pub node: Node<E>,
    codec: DurableCodec<'p>,
    opened: Opened,
    batch: u64,
    /// The latest synced tick with a WAL record, and the byte frontier covering it.
    synced: Option<SyncedTick>,
    /// The tick the latest checkpoint covers (from recovery, or taken here).
    checkpointed: Option<u64>,
}

impl<'p, E: Executor> ManualDriver<'p, E> {
    pub fn new(
        node: Node<E>,
        program: &'p Program,
        schema: &'p crate::durable::DurableSchema,
        names: std::sync::Arc<[std::sync::Arc<str>]>,
        opened: Opened,
    ) -> ManualDriver<'p, E> {
        ManualDriver {
            node,
            codec: DurableCodec::new(program, schema, names),
            batch: 0,
            synced: None,
            checkpointed: opened.checkpoint.map(|c| c.tick),
            opened,
        }
    }

    pub fn meta(&self) -> &MetaRecord {
        &self.opened.record
    }

    /// Runs ticks while the node is ready at `now`, making each durable before the next. Returns the released ticks.
    pub fn run_until_quiescent(&mut self, now: Instant) -> Result<Vec<ReleasedTick>, NodeError> {
        let mut out = Vec::new();
        self.run_until_quiescent_with(now, &mut |t| out.push(t))?;
        Ok(out)
    }

    /// [`ManualDriver::run_until_quiescent`], handing each tick to `sink` the moment it is released.
    pub fn run_until_quiescent_with(
        &mut self,
        now: Instant,
        sink: &mut dyn FnMut(ReleasedTick),
    ) -> Result<(), NodeError> {
        while self.node.ready(now)? {
            let fx = self.node.run_tick(now).map_err(|f| f.error)?;
            self.commit(&fx, sink)?;
        }
        Ok(())
    }

    /// Runs exactly one tick at `now` (whether or not the node is ready: an empty tick is legal, SEM-009).
    pub fn run_one(&mut self, now: Instant) -> Result<Vec<ReleasedTick>, NodeError> {
        let fx = self.node.run_tick(now).map_err(|f| f.error)?;
        let mut out = Vec::new();
        self.commit(&fx, &mut |t| out.push(t))?;
        Ok(out)
    }

    /// Fault injection: runs one tick at `now` up to its WAL append and stops before the sync, consuming the driver.
    /// The store is left as a crash in that window leaves it (the record written but not durable) and nothing of
    /// the tick was released; the caller crashes the filesystem next.
    pub fn crash_before_sync(mut self, now: Instant) -> Result<(), NodeError> {
        let fx = self.node.run_tick(now).map_err(|f| f.error)?;
        if let Some(r) = fx.reserve {
            self.opened.record.reserved_tick = r.ticks.0;
            self.opened.record.last_now = self.opened.record.last_now.max(r.now.0);
            MetaStore::write(&self.opened.meta, &self.opened.record)?;
        }
        if let Some(delta) = &fx.wal {
            self.append(&fx, delta)?;
        }
        Ok(())
    }

    /// Appends the tick's record (its blobs logged in it, or made durable as files first; FOREIGN-PROTOCOLS §5).
    fn append(&mut self, fx: &TickEffects, delta: &crate::durable::Delta) -> Result<(), NodeError> {
        self.batch = self
            .batch
            .checked_add(1)
            .ok_or_else(|| internal_error!("the WAL batch counter overflows"))?;
        let record = tick_record(&self.opened.blobs, &fx.blobs, self.codec.encode_delta(delta)?)?;
        let lsn = self.opened.wal.append(&WalRecordBuf {
            batch: self.batch,
            tick: fx.tick.0,
            now: fx.now.0,
            kind: record.kind,
            payload: record.payload,
        })?;
        self.opened.blobs.write_logged(&record.logged, lsn)?;
        Ok(())
    }

    fn commit(&mut self, fx: &TickEffects, sink: &mut dyn FnMut(ReleasedTick)) -> Result<(), NodeError> {
        if let Some(r) = fx.reserve {
            self.opened.record.reserved_tick = r.ticks.0;
            self.opened.record.last_now = self.opened.record.last_now.max(r.now.0);
            MetaStore::write(&self.opened.meta, &self.opened.record)?;
            self.node.reserved(r)?.into_iter().for_each(&mut *sink);
        }
        let Some(delta) = &fx.wal else {
            self.node.release_ready()?.into_iter().for_each(&mut *sink);
            return Ok(());
        };
        self.append(fx, delta)?;
        let synced = self.opened.wal.sync()?;
        let tick = synced
            .synced_tick()
            .ok_or_else(|| internal_error!("a sync after an append covers no tick"))?;
        self.synced = Some(tick);
        self.node
            .wal_synced(Tick(tick.tick()))?
            .into_iter()
            .for_each(&mut *sink);
        Ok(())
    }

    /// Checkpoints the durable rows at the synced frontier and truncates the WAL it covers. Requires every computed
    /// tick to be released (always true between calls of this driver).
    pub fn checkpoint(&mut self) -> Result<(), NodeError> {
        let Some(covers) = self.synced else {
            return Ok(());
        };
        // Nothing was written since the last checkpoint: it already covers this tick.
        if self.checkpointed == Some(covers.tick()) {
            return Ok(());
        }
        if self.node.parked() != 0 {
            return Err(internal_error!("checkpoint with ticks still parked").into());
        }
        let outside = self.node.checkpoint_candidates();
        // A delta layer when the change since the installed checkpoint is known and the chain has room; otherwise a
        // full image (FOREIGN-PROTOCOLS §6).
        let delta = self.node.take_checkpoint_delta();
        let written = (|| -> Result<_, NodeError> {
            let id = match delta {
                Some(d) if crate::durable::layer_fits(self.opened.checkpoints.chain()?) => {
                    let payload = self.codec.encode_delta(&d)?;
                    self.opened.checkpoints.write_layer(&payload, covers)?
                }
                _ => {
                    let snap = self.codec.encode_image(self.node.released_image())?;
                    self.opened.checkpoints.write(snap, covers)?
                }
            };
            Ok(self.opened.checkpoints.install(id)?)
        })();
        // The change was taken: if it did not become a checkpoint, the next one must be full.
        let token = written.inspect_err(|_| self.node.checkpoint_failed())?;
        // The blobs logged in the WAL about to go are made durable as files first.
        self.opened.blobs.sync_logged_below(token.lsn())?;
        self.opened.wal.truncate_through(token)?;
        self.opened.checkpoints.prune()?;
        self.checkpointed = Some(covers.tick());
        // Recovery starts from this checkpoint now: the blobs nothing can reach go.
        let gone = self.node.blob_garbage(Tick(covers.tick()), &outside);
        self.opened.blobs.delete(&gone)?;
        Ok(())
    }
}
