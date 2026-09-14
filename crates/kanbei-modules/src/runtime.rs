//! Per-generation actor (T19): each generation owns its Wasmtime store on a
//! dedicated thread. Activation, hot calls, token queries, and shutdown are all
//! mailbox messages, so the store has a single writer and disposal has a real
//! quiesce → deadline → force substrate (R-24/C-04, architecture.md "Unified
//! module lifecycle" / "Generations run as Luaur inside separate Wasmtime
//! instances off the main thread").
//!
//! Host imports do NOT run on this thread. kanbei-vm offloads every top-level
//! host import to a bounded `kb-host-call` worker and awaits it, so a *host
//! op* cannot wedge the actor. In-guest work (including a blocking WASI
//! syscall) runs on this thread and is bounded only by the guest's fuel/epoch
//! limits — wasmtime 48 has no cross-thread cancel — so a guest that blocks
//! outside those limits does wedge the actor and the drain detaches it.
//!
//! A request that hits its reply deadline is NOT cancelled: the queued command
//! still executes. Callers must treat `ActorError::Wedged` as "outcome
//! unknown", not "did not happen".

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kanbei_vm::{GuestError, Instance};

/// A generation id (equal to the vm token; never reused).
pub(crate) type GenerationId = u64;

/// Default drain budget: how long `shutdown` waits for the actor to quiesce
/// before detaching it (architecture.md:233).
pub(crate) const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Caller-side reply deadline for a direct (supervisor-free) call. It must
/// exceed the vm's host-import `call_timeout` (default 5s) so a legitimately
/// slow host op is not reported as wedged. Cross-generation `service_call`
/// does NOT use this — it runs inside the caller's host-import supervision and
/// uses `host::SERVICE_CALL_WAIT` instead.
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The service-call scope a generation is currently executing under (T20).
/// `depth` counts hops from the root invocation; `visited` is the per-path set
/// of generations already on the call chain (seeded with the root caller), so a
/// cycle like A→B→A is rejected before it can deadlock two actors; `deadline`
/// is the absolute instant the whole chain must finish by.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub depth: u32,
    pub visited: Vec<GenerationId>,
    pub deadline: Instant,
}

impl Scope {
    /// The root scope for a top-level invocation of `generation`: no hops yet,
    /// the caller is the only visited generation, and the chain must finish
    /// within the runtime's reply deadline.
    pub(crate) fn root(generation: GenerationId, deadline: Instant) -> Self {
        Self {
            depth: 0,
            visited: vec![generation],
            deadline,
        }
    }
}

enum Cmd {
    RunScript {
        source: String,
        reply: SyncSender<Result<(), GuestError>>,
        scope: Option<Scope>,
    },
    Hot {
        entry: String,
        args: String,
        reply: SyncSender<Result<String, GuestError>>,
        scope: Option<Scope>,
    },
    Shutdown {
        done: SyncSender<()>,
    },
}

/// Why a mailbox request did not produce a guest result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorError {
    /// The actor is gone (its store was dropped).
    Gone,
    /// The actor did not answer within the reply deadline (wedged).
    Wedged,
}

impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorError::Gone => write!(f, "generation actor is gone"),
            ActorError::Wedged => write!(f, "generation actor is wedged (no reply in time)"),
        }
    }
}

/// A handle to a generation's store-owning thread. The `Instance` (and its
/// `Store`) lives and drops on that thread; the shared `abandoned` counter
/// counts drains that gave up (an abandoned-drain event counter — a later
/// clean exit does not decrement it, so it is not a live-leak gauge).
pub struct GenerationRuntime {
    generation: GenerationId,
    tx: Sender<Cmd>,
    reply_timeout: Duration,
    join: Mutex<Option<JoinHandle<()>>>,
    /// Work commands the actor is currently executing (0 while idle). A status
    /// seam: it proves the actor has picked up a request before a drain.
    in_flight: Arc<AtomicUsize>,
    /// Shared abandoned-drain counter; incremented once per timed-out drain.
    abandoned: Arc<AtomicU64>,
    /// Set when the actor thread panicked (so a disposal does not claim a clean
    /// quiesce). Its store was still dropped during unwinding.
    panicked: AtomicBool,
    /// The scope of the command the actor is executing right now (T20). The
    /// kernel host reads this for the calling generation in `service_call`;
    /// the actor is blocked inside the call while it is set, so the read is
    /// stable for the call's duration.
    scope: Arc<Mutex<Option<Scope>>>,
}

impl GenerationRuntime {
    /// Spawn the actor that owns `instance`. `reply_timeout` bounds every
    /// caller's wait for a guest result (the guest's own fuel/epoch/budget
    /// limits are enforced inside `Instance`); `abandoned` is the shared
    /// abandoned-drain counter.
    pub(crate) fn spawn(
        generation: GenerationId,
        instance: Instance,
        reply_timeout: Duration,
        abandoned: Arc<AtomicU64>,
    ) -> std::io::Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let scope: Arc<Mutex<Option<Scope>>> = Arc::new(Mutex::new(None));
        // Fail closed on thread exhaustion: activation reports the io error
        // rather than aborting the process.
        let join = thread::Builder::new()
            .name(format!("kb-gen-{generation}"))
            .spawn({
                let in_flight = Arc::clone(&in_flight);
                let scope = Arc::clone(&scope);
                move || run(instance, rx, &in_flight, &scope)
            })?;
        Ok(Arc::new(Self {
            generation,
            tx,
            reply_timeout,
            join: Mutex::new(Some(join)),
            in_flight,
            abandoned,
            panicked: AtomicBool::new(false),
            scope,
        }))
    }

    /// Number of work commands the actor is executing right now (0 = idle).
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Run the activation script on the actor thread, waiting up to the
    /// runtime's default reply deadline. The invocation runs under a fresh root
    /// scope (this generation is the only hop on the chain).
    pub(crate) fn run_script(&self, source: &str) -> Result<Result<(), GuestError>, ActorError> {
        let scope = Scope::root(self.generation, Instant::now() + self.reply_timeout);
        self.request(self.reply_timeout, Some(scope), |reply, scope| {
            Cmd::RunScript {
                source: source.to_string(),
                reply,
                scope,
            }
        })
    }

    /// Call the generation's `kb_hot` on the actor thread under a fresh root
    /// scope, waiting up to the runtime's default reply deadline. This is the
    /// kernel/UI entry point; a cross-generation hop enters through
    /// [`Self::hot_within`] with the caller's child scope instead.
    pub fn hot(
        &self,
        entry: &str,
        args: &str,
    ) -> Result<Result<String, GuestError>, ActorError> {
        let scope = Scope::root(self.generation, Instant::now() + self.reply_timeout);
        self.hot_request(entry, args, self.reply_timeout, scope)
    }

    /// The scope the actor is executing under right now, if any (T20). While a
    /// scope is set the actor is blocked inside the guest call, so the
    /// host-import worker running `service_call` reads the scope of the call in
    /// flight, not a stale one.
    pub(crate) fn scope(&self) -> Option<Scope> {
        self.scope.lock().expect("scope lock poisoned").clone()
    }

    /// Cross-generation hop: call `kb_hot` under `scope`, the caller's child
    /// scope (which carries the chain's depth, visited set, and deadline). Wait
    /// at most `wait` for the reply — bounded by the remaining chain deadline
    /// (see `host::op_service_call`).
    pub(crate) fn hot_within(
        &self,
        entry: &str,
        args: &str,
        wait: Duration,
        scope: Scope,
    ) -> Result<Result<String, GuestError>, ActorError> {
        self.hot_request(entry, args, wait, scope)
    }

    fn hot_request(
        &self,
        entry: &str,
        args: &str,
        wait: Duration,
        scope: Scope,
    ) -> Result<Result<String, GuestError>, ActorError> {
        self.request(wait, Some(scope), |reply, scope| Cmd::Hot {
            entry: entry.to_string(),
            args: args.to_string(),
            reply,
            scope,
        })
    }

    /// Whether the actor thread panicked (only known after a join).
    pub(crate) fn panicked(&self) -> bool {
        self.panicked.load(Ordering::Acquire)
    }

    /// Best-effort, non-blocking stop request (for the vm's `retire` path, which
    /// runs on a worker and must not block on a drain).
    pub(crate) fn request_shutdown(&self) {
        let (done, _never) = mpsc::sync_channel(1);
        let _ = self.tx.send(Cmd::Shutdown { done });
    }

    /// Quiesce → deadline → force. Returns `true` when the actor exited within
    /// `deadline` (the store was dropped on the actor thread); `false` when it
    /// was abandoned — the thread is detached, the shared abandoned-drain
    /// counter records it, and the caller marks the disposal `forced`. A
    /// `Disconnected` reply means the actor already exited (a preceding
    /// `request_shutdown` or drain), which is a clean join, not a wedge. Force
    /// cannot acquire a wedged thread: wasmtime 48 has no cross-thread cancel.
    pub fn shutdown(&self, deadline: Duration) -> bool {
        let (done, wait) = mpsc::sync_channel(1);
        if self.tx.send(Cmd::Shutdown { done }).is_err() {
            // The actor already exited; joining is immediate.
            return self.join();
        }
        match wait.recv_timeout(deadline) {
            Ok(()) => self.join(),
            // The actor is still executing a command past the deadline.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.abandoned.fetch_add(1, Ordering::Relaxed);
                false
            }
            // The actor exited before answering (its receiver is gone).
            Err(mpsc::RecvTimeoutError::Disconnected) => self.join(),
        }
    }

    fn request<T>(
        &self,
        wait: Duration,
        scope: Option<Scope>,
        make: impl FnOnce(SyncSender<T>, Option<Scope>) -> Cmd,
    ) -> Result<T, ActorError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(make(tx, scope)).map_err(|_| ActorError::Gone)?;
        match rx.recv_timeout(wait) {
            Ok(value) => Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ActorError::Wedged),
            // The actor dropped the reply (went away mid-request).
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ActorError::Gone),
        }
    }

    fn join(&self) -> bool {
        if let Some(handle) = self.join.lock().expect("actor join lock poisoned").take()
            && handle.join().is_err()
        {
            self.panicked.store(true, Ordering::Release);
        }
        true
    }
}

impl std::fmt::Debug for GenerationRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationRuntime")
            .field("generation", &self.generation)
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

/// The actor loop: sole owner of the store for the thread's lifetime. It
/// publishes the scope of the command it is running on `scope` for the duration
/// of the call, then clears it.
fn run(
    mut instance: Instance,
    rx: Receiver<Cmd>,
    in_flight: &AtomicUsize,
    scope: &Mutex<Option<Scope>>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::RunScript {
                source,
                reply,
                scope: cmd_scope,
            } => {
                *scope.lock().expect("scope lock poisoned") = cmd_scope;
                in_flight.fetch_add(1, Ordering::AcqRel);
                let result = instance.run_script(&source);
                in_flight.fetch_sub(1, Ordering::AcqRel);
                *scope.lock().expect("scope lock poisoned") = None;
                let _ = reply.send(result);
            }
            Cmd::Hot {
                entry,
                args,
                reply,
                scope: cmd_scope,
            } => {
                *scope.lock().expect("scope lock poisoned") = cmd_scope;
                in_flight.fetch_add(1, Ordering::AcqRel);
                let result = instance.call_json(&entry, &args);
                in_flight.fetch_sub(1, Ordering::AcqRel);
                *scope.lock().expect("scope lock poisoned") = None;
                let _ = reply.send(result);
            }
            Cmd::Shutdown { done } => {
                let _ = done.send(());
                break;
            }
        }
    }
    // `instance` (and its store) drops here, on the actor thread.
}
