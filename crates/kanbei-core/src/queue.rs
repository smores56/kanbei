//! The durability queue (ratification-packet §3): the actor ACKs after
//! write() + enqueue; one background thread executes fsync/dirsync strictly
//! FIFO; effect dispatch and terminal facts call [`DurabilityQueue::flush`]
//! and wait. Ordering invariant R-10: an object dirsync is enqueued before
//! the event frame that references it, so the object is durable before the
//! frame is fsync-durable.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{bounded, unbounded, Sender};

/// A durability op handed to the queue: sync a file's data+metadata, or sync
/// a directory entry (object install via rename).
pub enum SyncOp {
    Fsync(File),
    Dirsync(PathBuf),
}

impl SyncOp {
    fn run(&self) -> io::Result<()> {
        match self {
            SyncOp::Fsync(f) => f.sync_all(),
            SyncOp::Dirsync(dir) => File::open(dir).and_then(|d| d.sync_all()),
        }
    }
}

enum Job {
    Op(SyncOp),
    /// Barrier: the worker acks only after every op enqueued before it has run.
    Flush { done: Sender<()> },
}

/// Shared by handle and worker; holds the first op error seen so far.
type Errors = Arc<Mutex<Option<io::Error>>>;

/// One background thread, strictly FIFO ops, barrier flushes. Send+Sync;
/// callers share it via `Arc`.
pub struct DurabilityQueue {
    jobs: Sender<Job>,
    errors: Errors,
    // JoinHandle is !Sync, so the handle lives behind a mutex
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DurabilityQueue {
    pub fn start(thread_name: &str) -> Self {
        let (jobs, rx) = unbounded::<Job>();
        let errors: Errors = Arc::new(Mutex::new(None));
        let worker_errors = Arc::clone(&errors);
        let thread = std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || worker_loop(rx, worker_errors))
            .expect("durability queue: failed to spawn worker thread");
        Self {
            jobs,
            errors,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// Handoff only: never blocks on the op itself. Fails only when the
    /// worker thread is gone (channel disconnected).
    pub fn enqueue(&self, op: SyncOp) -> io::Result<()> {
        self.jobs
            .send(Job::Op(op))
            .map_err(|e| io::Error::other(format!("durability queue: worker unavailable: {e}")))
    }

    /// Barrier: waits until every op enqueued before this call has completed.
    /// Returns the first op error encountered (each error is surfaced once);
    /// a dead worker thread surfaces as an error too.
    pub fn flush(&self) -> io::Result<()> {
        let (done, rx) = bounded(1);
        self.jobs
            .send(Job::Flush { done })
            .map_err(|e| io::Error::other(format!("durability queue: worker unavailable: {e}")))?;
        if rx.recv().is_err() {
            return Err(io::Error::other("durability queue: worker thread died"));
        }
        match self.errors.lock().unwrap().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Flush, then stop the worker and join it. The worker exits because
    /// dropping the sender disconnects the channel.
    pub fn shutdown(self) -> io::Result<()> {
        let flushed = self.flush();
        drop(self.jobs);
        let joined = match self.thread.lock().unwrap().take() {
            Some(handle) => handle
                .join()
                .map_err(|_| io::Error::other("durability queue: worker thread panicked")),
            None => Ok(()),
        };
        flushed.and(joined)
    }
}

fn worker_loop(rx: crossbeam_channel::Receiver<Job>, errors: Errors) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Op(op) => {
                // FIFO order is guaranteed by the channel; only the first
                // error is kept, later ones are coalesced into it
                if let Err(e) = op.run() {
                    let mut guard = errors.lock().unwrap();
                    if guard.is_none() {
                        *guard = Some(e);
                    }
                }
            }
            Job::Flush { done } => {
                let _ = done.send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmp_file(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kb-core-queue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn fsync_flush_persists_data() {
        let q = DurabilityQueue::start("test-fsync");
        let path = tmp_file("fsync");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"hello durable world").unwrap();
        q.enqueue(SyncOp::Fsync(f)).unwrap();
        q.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello durable world");
        q.shutdown().unwrap();
    }

    #[test]
    fn flush_waits_for_all_prior_ops() {
        let q = DurabilityQueue::start("test-order");
        let path = tmp_file("order");
        let mut w = File::create(&path).unwrap();
        let mut expected = Vec::new();
        for i in 0..100 {
            let chunk = format!("chunk {i}\n");
            w.write_all(chunk.as_bytes()).unwrap();
            expected.extend_from_slice(chunk.as_bytes());
            q.enqueue(SyncOp::Fsync(w.try_clone().unwrap())).unwrap();
        }
        q.flush().unwrap();
        drop(w);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        q.shutdown().unwrap();
    }

    #[test]
    fn flush_with_no_pending_ops_returns_ok() {
        let q = DurabilityQueue::start("test-empty-flush");
        q.flush().unwrap();
        q.shutdown().unwrap();
    }

    #[test]
    fn flush_is_a_barrier_not_a_drain() {
        let q = DurabilityQueue::start("test-barrier");
        let path = tmp_file("barrier");
        let mut w = File::create(&path).unwrap();
        let mut expected = Vec::new();
        for i in 0..20 {
            let chunk = format!("chunk {i}\n");
            w.write_all(chunk.as_bytes()).unwrap();
            expected.extend_from_slice(chunk.as_bytes());
            q.enqueue(SyncOp::Fsync(w.try_clone().unwrap())).unwrap();
        }
        // first flush covers only the 20 ops above; the batch enqueued after
        // it is not waited on (barrier semantics, not drain)
        q.flush().unwrap();
        for i in 20..40 {
            let chunk = format!("chunk {i}\n");
            w.write_all(chunk.as_bytes()).unwrap();
            expected.extend_from_slice(chunk.as_bytes());
            q.enqueue(SyncOp::Fsync(w.try_clone().unwrap())).unwrap();
        }
        q.flush().unwrap();
        drop(w);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        q.shutdown().unwrap();
    }

    #[test]
    fn dirsync_op() {
        let q = DurabilityQueue::start("test-dirsync");
        q.enqueue(SyncOp::Dirsync(std::env::temp_dir())).unwrap();
        q.flush().unwrap();
        q.shutdown().unwrap();
    }

    #[test]
    fn shutdown_flushes_then_joins() {
        let q = DurabilityQueue::start("test-shutdown");
        let path = tmp_file("shutdown");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"data").unwrap();
        q.enqueue(SyncOp::Fsync(f)).unwrap();
        q.shutdown().unwrap();
        // no explicit flush: shutdown must flush before joining
        assert_eq!(std::fs::read(&path).unwrap(), b"data");
    }
}
