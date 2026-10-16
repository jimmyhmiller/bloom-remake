//! One asynchronous reservation writer. Acknowledgements follow atomic_write's directory sync;
//! a failed write terminates the writer, so no later request can mask uncertain persistence.
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;

use blossom_node::node::Reservation;
use blossom_store::{MetaRecord, MetaStore, StoreLock};

use crate::RuntimeError;

type Completion = Result<Reservation, String>;

pub(crate) fn start(
    meta: MetaStore,
    mut record: MetaRecord,
    lock: StoreLock,
    completed: impl FnMut(Completion) + Send + 'static,
) -> Result<(SyncSender<Reservation>, JoinHandle<()>), RuntimeError> {
    let (tx, rx) = mpsc::sync_channel(1);
    let thread = crate::server::spawn("metadata", move || {
        let _lock = lock;
        run(
            rx,
            |r| {
                record.reserved_tick = record.reserved_tick.max(r.ticks.0);
                record.last_now = record.last_now.max(r.now.0);
                meta.write(&record).map_err(|e| e.to_string())
            },
            completed,
        );
    })?;
    Ok((tx, thread))
}

fn run(
    requests: Receiver<Reservation>,
    mut persist: impl FnMut(Reservation) -> Result<(), String>,
    mut completed: impl FnMut(Completion),
) {
    while let Ok(r) = requests.recv() {
        let result = persist(r).map(|()| r);
        let failed = result.is_err();
        completed(result);
        if failed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::time::{Instant, Tick};

    #[test]
    fn acknowledges_only_after_persistence_and_drains_on_shutdown() {
        let r = Reservation {
            ticks: Tick(42),
            now: Instant(123),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let (began_tx, began_rx) = mpsc::channel();
        let (durable_tx, durable_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            run(
                rx,
                |request| {
                    began_tx.send(request).unwrap();
                    durable_rx.recv().unwrap();
                    Ok(())
                },
                |result| done_tx.send(result).unwrap(),
            )
        });
        tx.try_send(r).unwrap();
        assert_eq!(began_rx.recv().unwrap(), r);
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        drop(tx); // shutdown must not abandon an in-flight write
        durable_tx.send(()).unwrap();
        assert_eq!(done_rx.recv().unwrap(), Ok(r));
        thread.join().unwrap();
        assert!(done_rx.recv().is_err());
    }

    #[test]
    fn failure_is_reported_and_no_subsequent_request_is_written() {
        let r = Reservation {
            ticks: Tick(42),
            now: Instant(123),
        };
        let (tx, rx) = mpsc::channel();
        tx.send(r).unwrap();
        tx.send(r).unwrap();
        drop(tx);
        let mut writes = 0;
        let mut reports = Vec::new();
        run(
            rx,
            |_| {
                writes += 1;
                Err("sync failed".into())
            },
            |r| reports.push(r),
        );
        assert_eq!(writes, 1);
        assert_eq!(reports, vec![Err("sync failed".into())]);
    }
}
