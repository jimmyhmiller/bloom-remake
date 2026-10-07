//! A synchronous driver (ARCHITECTURE §5.10 `ManualDriver`): runs a node's ticks on the calling thread, appending
//! and syncing each tick's WAL record before running the next, and applying each released tick to the node's
//! database (docs/design/DATABASE.md), which it flushes when it grows (or when asked), truncating the WAL behind it.
//! No pipelining: it is the simplest correct driver, for tests, tools, the simulator and embedding without threads.
//! The network runtime pipelines the same node (`blossom-runtime`).

use blossom_base::internal_error;
use blossom_ir::core::Program;
use blossom_store::{MetaRecord, MetaStore, WalRecordBuf, WalWriter};
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
            opened,
        }
    }

    pub fn meta(&self) -> &MetaRecord {
        &self.opened.record
    }

    /// The node's database.
    pub fn database(&self) -> &crate::database::Database {
        &self.opened.database
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
            let released = self.node.reserved(r)?;
            self.release(released, sink)?;
        }
        let released = match &fx.wal {
            None => self.node.release_ready()?,
            Some(delta) => {
                self.append(fx, delta)?;
                let synced = self.opened.wal.sync()?;
                let tick = synced
                    .synced_tick()
                    .ok_or_else(|| internal_error!("a sync after an append covers no tick"))?;
                self.node.wal_synced(Tick(tick.tick()))?
            }
        };
        self.release(released, sink)?;
        if self.opened.database.needs_flush()? {
            self.flush()?;
        }
        Ok(())
    }

    /// Hands released ticks on, each applied to the database first (it holds only released ticks).
    fn release(&mut self, ticks: Vec<ReleasedTick>, sink: &mut dyn FnMut(ReleasedTick)) -> Result<(), NodeError> {
        for t in ticks {
            self.opened.database.apply(t.tick.0, &t.delta)?;
            sink(t);
        }
        Ok(())
    }

    /// Flushes the database (every released tick is in it: this driver releases each tick before the next runs),
    /// truncates the WAL behind it, and deletes the blobs no recovery from it can reach and the node no longer needs.
    pub fn flush(&mut self) -> Result<(), NodeError> {
        if self.node.parked() != 0 {
            return Err(internal_error!("a database flush with ticks still parked").into());
        }
        let outside = self.node.collection_candidates();
        let flushed = self.opened.database.flush()?;
        // The blobs logged in the records the tables now cover become files (the rows that hold them are in the
        // tables), and the WAL segments the tables wholly cover go.
        if let Some(lsn) = self.opened.wal.covered(&flushed)? {
            self.opened.blobs.sync_logged_below(lsn)?;
        }
        if let Some(token) = self.opened.wal.truncation(&flushed)? {
            self.opened.wal.truncate_through(token)?;
        }
        if let Some(t) = flushed.version() {
            // Recovery starts from these tables now: the blobs nothing can reach go.
            let gone = self.node.blob_garbage(Tick(t), &outside);
            self.opened.blobs.delete(&gone)?;
        }
        Ok(())
    }
}
