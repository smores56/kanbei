//! M2 milestone gate tests: the acceptance bullets and consistency tests M2
//! exercises (docs/architecture.md lines 629-663) — crash injection at the
//! module seams (633), generation replacement leaves no stale state (634),
//! wasm traps do not corrupt the session (635), capabilities attenuate and
//! stale generations cannot act (636), no-effect policy plugins (637), config
//! reload publishes atomically (640) — and consistency tests 1 Owner, 2
//! Authority, 9 Privacy, 10 Replay honesty, 15 Scope.
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from
//! the workspace root first; module-dependent tests print `skip:` and pass
//! without it (the suite must stay green either way).

use std::path::{Path, PathBuf};
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kanbei_capabilities::{
    BrokerError, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::registry::Registry;
use kanbei_log::{for_each_frame, recover};
use kanbei_modules::{ModuleError, ModuleOrigin, PackageManifest};
use kanbei_objects::ObjectStore;
use kanbei_policy::builtins::{PatternRedactionPolicy, StoreAllPolicy};
use kanbei_policy::{Admission, Candidate, CandidateRole, PolicyError, PolicyPlugin, RetentionDecision};
use kanbei_projection::reconstruct;
use kanbei_scopes::errors::ScopeError;
use kanbei_scopes::registry::ContributionRegistry;
use kanbei_scopes::scope_tree::{OwnerLease, ScopeTree};
use kanbei_services::{ScopePath, ServiceDependency, ServiceKey, ServiceRegistry};
use kanbei_session::{FaultPoint, NewEvent, Session, SessionConfig, SessionError};
use kanbei_testkit::{child_acked, session_dir_layout, spawn_m2_crash_child, verify_m2_recovery};
use kanbei_vm::{GuestError, Host, Vm, VmConfig};
use serde_json::{Value, json};

// --- helpers ---------------------------------------------------------------

fn fresh_session_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kanbei-gate-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Best-effort cleanup at test end; never fail a test on cleanup errors.
struct DirGuard(PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_store(dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>) {
    let queue = Arc::new(DurabilityQueue::start("kb-gate-m2-store"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    (store, queue)
}

fn shutdown_store(store: ObjectStore, queue: Arc<DurabilityQueue>) {
    drop(store);
    let q = Arc::try_unwrap(queue).unwrap_or_else(|_| panic!("store queue still shared"));
    q.shutdown().unwrap();
}

/// Module tests need the guest wasm; without it they skip with a note.
fn require_guest() -> bool {
    match Vm::load(no_epoch()) {
        Ok(_) => true,
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

/// Non-fuel, non-epoch-bounded engine config for tests whose modules must not
/// trap (the trap test uses the m2.rs recipe's fuel budget instead).
fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

fn root() -> ScopePath {
    ScopePath(vec![])
}

fn svc_key(name: &str) -> ServiceKey {
    ServiceKey {
        scope: root(),
        name: name.into(),
    }
}

fn manifest(id: Id128, source: &str, deps: Vec<ServiceDependency>) -> PackageManifest {
    manifest_trust(id, source, deps, TrustClass::User)
}

fn manifest_trust(
    id: Id128,
    source: &str,
    deps: Vec<ServiceDependency>,
    trust_class: TrustClass,
) -> PackageManifest {
    PackageManifest {
        schema: 1,
        module_id: id,
        origin: ModuleOrigin::UserConfig,
        trust_class,
        scope: root(),
        deps,
        capabilities: vec![],
        source: source.to_string(),
        state_schema: None,
    }
}

fn event(kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        kind: kind.into(),
        payload_schema: 1,
        payload,
        objects: Vec::new(),
        refs: Vec::new(),
    }
}

/// All envelopes currently in the log, in seq order.
fn envelopes(log_path: &Path) -> Vec<kanbei_core::envelope::Envelope> {
    let mut out = Vec::new();
    for_each_frame(log_path, |frame| {
        for line in &frame.events {
            out.push(kanbei_core::envelope::Envelope::from_line(line).unwrap());
        }
    })
    .unwrap();
    out
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// --- guest sources (the M2 Luau contract, shapes from tests/m2.rs) ---------

/// Publishes `greet` v1 and writes module state from kb_on_activate.
const GREET_PUBLISHER: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greet"}', 1, '[]')
  ctx.state_set('counter', 1, '{"n":1}')
end
function kb_hot(x) return { from = "greet", got = x } end
"#;

/// Publishes `greet` v2 (generation replacement).
const GREET_REPLACER: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greet"}', 2, '[]')
end
function kb_hot(x) return { from = "replacer", got = x } end
"#;

/// Publishes `greet` v1, then fails — a config that conflicts with a prior
/// holder AND fails on a fresh registry (safe mode on reopen, R-01/C-02).
const CONFLICT_AND_FAIL: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greet"}', 1, '[]')
  error('boom')
end
function kb_hot(x) return x end
"#;

/// Publishes nothing; used as the caller side of effect dispatch.
const CALLER: &str = r#"
function kb_on_activate(ctx) end
function kb_hot(x) return x end
"#;

/// Publishes `trap` v1; kb_hot burns fuel forever (trap containment test).
const TRAP: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"trap"}', 1, '[]')
end
function kb_hot(x)
  local n = 0
  while true do n = n + 1 end
end
"#;

// --- acceptance: generation replacement leaves no stale state (634) --------
// consistency 1 Owner + 15 Scope

#[test]
fn acceptance_generation_replacement_leaves_no_stale_state() {
    if !require_guest() { return; }
    let dir = fresh_session_dir("m2-owner");
    let _guard = DirGuard(dir.clone());
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id, GREET_PUBLISHER, vec![])),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(session.composition().epoch, 1);
    let generation_a = session.modules().unwrap().snapshot()[0].1;
    assert_eq!(generation_a, 1);
    // A's kb_on_activate wrote module state; the head holds the write
    let (head_a, bytes) = session
        .modules()
        .unwrap()
        .state()
        .lock()
        .unwrap()
        .get("counter")
        .unwrap()
        .unwrap();
    assert_eq!(bytes, br#"{"n":1}"#);
    assert_eq!(head_a.seq, 1);

    // replacement B: same module id, different source, same service name
    let outcome = session.replace_module(id, manifest(id, GREET_REPLACER, vec![])).unwrap();
    assert_eq!(outcome.old.generation, 1);
    assert_eq!(outcome.new.generation, 2);
    drop(outcome);

    // exactly one svc.greet provider, and it is B's generation
    let snapshot = session.modules().unwrap().services().lock().unwrap().snapshot();
    let greet: Vec<_> = snapshot.iter().filter(|(k, _, _)| k.name == "greet").collect();
    assert_eq!(greet.len(), 1, "stale registrations survive: {snapshot:?}");
    assert_eq!(greet[0].1.generation, 2);

    // A's generation token is stale: it can neither dispatch effects nor
    // update module state (R-02/C-03)
    let err = session.effect_dispatch(&svc_key("greet"), "{}", generation_a).unwrap_err();
    assert!(matches!(err, SessionError::StaleGeneration { generation: 1 }));
    let err = session
        .module_state_cas("counter", 1, b"x".to_vec(), generation_a)
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::State(kanbei_modules::StateError::StaleGeneration { generation: 1 })
    ));

    // the scope tree holds no scopes beyond the root — the module's scope "/"
    // IS the root (R-26: no per-module scope leakage)
    let scopes = session.scopes().scopes();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].path, root());
    assert!(scopes[0].children.is_empty());

    // the log's composition_changed events record the delta (added/removed)
    let log = dir.join("log.zst");
    let envs = envelopes(&log);
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[0].kind, "composition_changed");
    assert_eq!(envs[0].payload["delta"]["added"][0]["generation"], 1);
    assert_eq!(envs[0].payload["delta"]["removed"], json!([]));
    assert_eq!(envs[1].kind, "composition_changed");
    assert_eq!(envs[1].payload["delta"]["removed"][0]["generation"], 1);
    assert_eq!(envs[1].payload["delta"]["added"][0]["generation"], 2);

    // audit reconstruction accounts both events and finds no missing objects
    session.close().unwrap();
    let (store, queue) = open_store(&dir);
    let report = reconstruct(&log, &Registry::new(), &store).unwrap();
    assert_eq!(report.kinds["composition_changed"].count, 2);
    assert!(report.missing_objects.is_empty());
    shutdown_store(store, queue);
}

// --- acceptance: wasm traps do not corrupt the session (635) ---------------

#[test]
fn acceptance_wasm_traps_do_not_corrupt_session() {
    if !require_guest() { return; }
    let dir = fresh_session_dir("m2-trap");
    let _guard = DirGuard(dir.clone());
    let id_trap = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        // the m2.rs trap recipe: 50M fuel — enough for the activation shim,
        // while kb_hot's loop deterministically exhausts the per-call budget
        engine: Some(VmConfig {
            fuel_per_call: 50_000_000,
            epoch_deadline: u64::MAX,
            ..Default::default()
        }),
        config: Some(manifest(id_trap, TRAP, vec![])),
        ..Default::default()
    })
    .unwrap();
    let dep = ServiceDependency {
        key: svc_key("trap"),
        required_version: 1,
    };
    let id_caller = Id128::generate();
    let caller = session.activate_config(manifest(id_caller, CALLER, vec![dep])).unwrap();
    let err = session.effect_dispatch(&svc_key("trap"), "{}", caller.generation).unwrap_err();
    assert!(matches!(err, SessionError::Effect(_)), "got {err:?}");
    // the session actor still commits, recovery is clean, reopening works
    let receipt = session.commit(vec![event("post-trap", json!({"n": 1}))], None).unwrap();
    assert_eq!(receipt.first_seq, 3);
    session.close().unwrap();
    let recovered = recover(&dir.join("log.zst")).unwrap();
    assert_eq!(recovered.events, 3);
    let session2 = Session::open(SessionConfig { dir: dir.to_path_buf(), ..Default::default() }).unwrap();
    assert_eq!(session2.next_seq(), 4);
    session2.close().unwrap();
}

// --- acceptance: capabilities attenuate and stale generations cannot act ---
// (636) consistency 2 Authority

#[test]
fn acceptance_capabilities_attenuate_and_stale_cannot_act() {
    if !require_guest() { return; }
    let dir = fresh_session_dir("m2-authority");
    let _guard = DirGuard(dir.clone());
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest_trust(id, CALLER, vec![], TrustClass::Agent)),
        ..Default::default()
    })
    .unwrap();
    let generation = session.modules().unwrap().snapshot()[0].1;
    let host = session.modules().unwrap().host();

    // template: Agent may read state, never write it — deny wins over grant
    // coverage (R-13/D-04: templates keyed by origin trust class)
    let principal = Principal {
        session: host.session(),
        generation,
        run: None,
    };
    {
        let mut broker = host.broker().lock().unwrap();
        broker
            .add_template(PolicyTemplate {
                trust_class: TrustClass::Agent,
                allow: vec![Capability::new("state".into(), vec!["read".into()])],
                deny: vec![Capability::new("state".into(), vec!["write".into()])],
                require_approval: vec![],
                monotonic: false,
                version: 1,
            })
            .unwrap();
        let mut grant = Grant {
            principal: principal.clone(),
            module_generation: generation,
            capability: Capability::new("state".into(), vec!["read".into(), "write".into()]),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: None,
            policy_version: 1,
            grant_digest: Digest::new(b"placeholder"),
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
        // the grant covers write, but the deny guard still rejects it
        let err = broker
            .check(&principal, &Capability::new("state".into(), vec!["write".into()]), 1)
            .unwrap_err();
        assert!(matches!(err, BrokerError::Denied { verb, .. } if verb == "write"));
        // read passes the guards
        broker
            .check(&principal, &Capability::new("state".into(), vec!["read".into()]), 1)
            .unwrap();
        // attenuation only narrows: fs.read+write attenuates to fs.read
        let attenuated = broker.attenuate(
            &Capability::new("fs".into(), vec!["read".into(), "write".into()]),
            &["write".into()],
        );
        assert_eq!(attenuated, Capability::new("fs".into(), vec!["read".into()]));
    }

    // the module's host-op seam surfaces the same denial: a state-write
    // capability request is rejected (op 4 = check; op 2 state_set is
    // generation-gated, so the broker denial is exercised at the check seam —
    // dispatch-time re-verification, R-16/D-11)
    let err = host
        .call(generation, 4, r#"{"resource":"state","verbs":["write"]}"#)
        .unwrap_err();
    assert!(err.contains("denied state/write"), "got {err:?}");
    let ok = host
        .call(generation, 4, r#"{"resource":"state","verbs":["read"]}"#)
        .unwrap();
    assert_eq!(ok, r#"{"allowed":true}"#);
    // the current generation's state write works (generation-gated, not
    // broker-gated at the op level)
    host.call(generation, 2, r#"{"key":"k","schema":1,"value":{"n":1}}"#).unwrap();

    // stale generations cannot act: after replacement the old token is dead —
    // both the check seam and the state path reject it and the host records
    // the rejected stale effect (R-02/C-03)
    let outcome = session.replace_module(id, manifest_trust(id, CALLER, vec![], TrustClass::Agent)).unwrap();
    drop(outcome);
    let err = host.call(generation, 4, r#"{"resource":"state","verbs":["read"]}"#).unwrap_err();
    assert!(err.contains("stale generation"), "got {err:?}");
    let err = host.call(generation, 2, r#"{"key":"k","schema":1,"value":{"n":2}}"#).unwrap_err();
    assert!(err.contains("stale generation"), "got {err:?}");
    assert_eq!(host.rejected_stale_effects(), 2);

    drop(host);
    session.close().unwrap();
}

// --- acceptance: config reload publishes atomically (640) ------------------

#[test]
fn acceptance_config_reload_publishes_atomically() {
    if !require_guest() { return; }
    let dir = fresh_session_dir("m2-config");
    let _guard = DirGuard(dir.clone());
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();

    // (a) success: epoch bumps, the composition_changed event lands, the
    // service is resolvable
    let id_a = Id128::generate();
    let activation = session.activate_config(manifest(id_a, GREET_PUBLISHER, vec![])).unwrap();
    assert_eq!(activation.epoch, 1);
    assert_eq!(session.composition().epoch, 1);
    let provider = session
        .modules()
        .unwrap()
        .services()
        .lock()
        .unwrap()
        .resolve(&svc_key("greet"), 1, &root())
        .unwrap()
        .clone();
    assert_eq!(provider.module_id, id_a);

    // (b) a conflicting config (same key, different provider) fails
    // atomically: Err, epoch unchanged, no new event, module absent
    let id_b = Id128::generate();
    let err = session.activate_config(manifest(id_b, CONFLICT_AND_FAIL, vec![])).unwrap_err();
    assert!(matches!(err, SessionError::Module(ModuleError::Activation(_))), "got {err:?}");
    assert_eq!(session.composition().epoch, 1);
    let snapshot = session.modules().unwrap().snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].0, id_a);
    let envs = envelopes(&dir.join("log.zst"));
    assert_eq!(envs.len(), 1, "failed activation must not commit");
    assert_eq!(envs[0].kind, "composition_changed");
    session.close().unwrap();

    // (c) reopening with the same failing config activates built-in safe mode
    // (R-01/C-02): the config's kb_on_activate fails even on a fresh registry
    // (it errors after publishing), so modules drop and a canonical
    // safe_mode_activated event is committed; the session stays usable
    let mut session2 = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id_b, CONFLICT_AND_FAIL, vec![])),
        ..Default::default()
    })
    .unwrap();
    assert!(session2.modules().is_none());
    assert_eq!(session2.vm_engine_digest(), None);
    let envs = envelopes(&dir.join("log.zst"));
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[1].kind, "safe_mode_activated");
    let receipt = session2.commit(vec![event("post-safe", json!({"n": 1}))], None).unwrap();
    assert_eq!(receipt.first_seq, 3);
    session2.close().unwrap();
}

// --- acceptance: no-effect policy plugins (637) ----------------------------
// consistency 9 Privacy + 10 Replay honesty (boundary-fact assertions; the
// reconstruction assertion lives in consistency_10_replay_honesty_explicit_boundary)

#[test]
fn acceptance_retention_policy_no_effect() {
    // Structural: the PolicyPlugin seam exposes no effect capabilities — its
    // only methods are decide/name/is_no_effect, and decide is a pure
    // content -> decision function over a bounded candidate. Builtins are
    // no-effect by construction (the deferred R-28/D-S3 runtime hosts the
    // same trait behind an empty capability import set); the gate proof is
    // that decisions are pure functions with no I/O surface to invoke
    // effects with.
    assert!(StoreAllPolicy.is_no_effect());
    assert!(PatternRedactionPolicy::new(vec![], "[redacted]".into()).unwrap().is_no_effect());

    // Privacy: redaction transforms before storage; no SECRET bytes reach the
    // log or the object store
    let dir = fresh_session_dir("m2-privacy");
    let _guard = DirGuard(dir.clone());
    let policy = Arc::new(
        PatternRedactionPolicy::new(vec!["SECRET".into()], "[redacted]".into()).unwrap(),
    );
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        policy,
        ..Default::default()
    })
    .unwrap();
    let adm = session
        .retain_candidate(Candidate {
            role: CandidateRole::ModelContext,
            content: b"contains SECRET here".to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert_eq!(adm, Admission::Stored { bytes: b"contains [redacted] here".to_vec() });
    // the candidate never touched storage: only the genesis manifest object
    // exists and no file under the session dir carries the secret
    assert_eq!(session.store().scan().unwrap().len(), 1);
    for entry in session_dir_layout(&dir) {
        if entry.ends_with('/') {
            continue;
        }
        let bytes = std::fs::read(dir.join(&entry)).unwrap();
        assert!(!contains_subslice(&bytes, b"SECRET"), "file {entry} leaks SECRET");
    }
    session.close().unwrap();

    // Replay honesty: Drop on a replay-relevant candidate forces the explicit
    // non-resumable boundary + the canonical retention_boundary fact
    struct DropPlugin;
    impl PolicyPlugin for DropPlugin {
        fn decide(&self, _c: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::Drop { reason: "no-store".into() })
        }
        fn name(&self) -> &'static str {
            "drop-all"
        }
    }
    let dir2 = fresh_session_dir("m2-replay");
    let _guard2 = DirGuard(dir2.clone());
    let mut session2 = Session::open(SessionConfig {
        dir: dir2.to_path_buf(),
        policy: Arc::new(DropPlugin),
        ..Default::default()
    })
    .unwrap();
    let adm = session2
        .retain_candidate(Candidate {
            role: CandidateRole::ModelContext,
            content: b"data".to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert!(matches!(adm, Admission::NonResumableBoundary { .. }));
    let envs = envelopes(&dir2.join("log.zst"));
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].kind, "retention_boundary");
    assert_eq!(envs[0].payload["reason"], "no-store");
    assert_eq!(envs[0].payload["replay_relevant"], true);
    assert_eq!(envs[0].payload["kind"], "non_resumable");
    // Internal candidates are never replay-relevant (R-04): Dropped, and no
    // boundary fact is committed
    let adm = session2
        .retain_candidate(Candidate {
            role: CandidateRole::Internal,
            content: b"data".to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert!(matches!(adm, Admission::Dropped { .. }));
    assert_eq!(envelopes(&dir2.join("log.zst")).len(), 1);
    session2.close().unwrap();
}

// --- acceptance: crash injection at the M2 seams (633) ---------------------

#[test]
fn acceptance_crash_m2_points() {
    if !require_guest() { return; }
    const POINTS: [FaultPoint; 6] = [
        FaultPoint::BeforeConfigActivation,
        FaultPoint::AfterConfigActivation,
        FaultPoint::BeforeEffectDispatch,
        FaultPoint::AfterEffectDispatch,
        FaultPoint::BeforeHeadUpdate,
        FaultPoint::AfterHeadUpdate,
    ];
    for point in POINTS {
        for flow in ["head", "dispatch"] {
            let dir = fresh_session_dir(&format!("m2-crash-{flow}"));
            let _guard = DirGuard(dir.clone());
            let mut child = spawn_m2_crash_child(&dir, Some(point), 3, 6, flow);
            let status = child.wait().unwrap();
            let acked = child_acked(&mut child);
            // The abort fires only where the flow reaches the seam: config
            // points fire inside open, head points at the CAS updates, and
            // the pre-dispatch point only in the dispatch flow. The
            // post-dispatch point can never fire: the child's dispatch is
            // rejected by contract (the config generation calling its own
            // service — re-entrant instance lock), so the dispatch errors
            // before AfterEffectDispatch is reached.
            let fires = match point {
                FaultPoint::BeforeConfigActivation
                | FaultPoint::AfterConfigActivation
                | FaultPoint::BeforeHeadUpdate
                | FaultPoint::AfterHeadUpdate => true,
                FaultPoint::BeforeEffectDispatch => flow == "dispatch",
                FaultPoint::AfterEffectDispatch => false,
                // the M2 child never configures the M1 commit-path points
                FaultPoint::BeforeObjectInstall
                | FaultPoint::AfterObjectInstall
                | FaultPoint::BeforeFrameAppend
                | FaultPoint::AfterFrameAppend
                // M3 spine points never fire in the M2 child
                | FaultPoint::BeforeWakeAccept
                | FaultPoint::AfterWakeAccept
                | FaultPoint::BeforeRunStart
                | FaultPoint::AfterRunStart
                | FaultPoint::BeforeModelCall
                | FaultPoint::AfterModelCall
                | FaultPoint::BeforeToolIntentCommit
                | FaultPoint::AfterToolIntentCommit
                | FaultPoint::BeforeToolDispatch
                | FaultPoint::AfterToolDispatch
                | FaultPoint::BeforeToolOutcomeCommit
                | FaultPoint::AfterToolOutcomeCommit
                | FaultPoint::BeforeRunOutcome
                | FaultPoint::AfterRunOutcome
                // M4 memory points never fire in the M2 child
                | FaultPoint::BeforeMemoryProposal
                | FaultPoint::AfterMemoryProposal => false,
            };
            if fires {
                assert_eq!(
                    status.signal(),
                    Some(6),
                    "{point:?} {flow}: child must abort (SIGABRT), exited {status:?}"
                );
            } else {
                assert!(status.success(), "{point:?} {flow}: child must complete, got {status:?}");
            }
            verify_m2_recovery(&dir, acked).unwrap_or_else(|e| panic!("{point:?} {flow}: {e}"));
            let rec = recover(&dir.join("log.zst")).unwrap();
            println!(
                "m2 {point:?} {flow:>8}: acked={acked} R={} truncated={} crashed={fires}",
                rec.events, rec.truncated
            );
        }
    }
}

// --- consistency 15: scope lifecycle + ephemeral scopes --------------------

#[test]
fn consistency_15_scope_lifecycle_and_ephemeral() {
    let dir = fresh_session_dir("m2-scope");
    let _guard = DirGuard(dir.clone());
    let session = Session::open(SessionConfig { dir: dir.to_path_buf(), ..Default::default() }).unwrap();
    // root exists
    let scopes = session.scopes().scopes();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].path, root());
    assert_eq!(scopes[0].owner, OwnerLease::Run(0));

    // the lifecycle contract on a standalone tree (the session's tree is
    // read-only by design — scope publishes go through the composition's
    // staged/validated publish, R-26/C-09)
    let mut tree = ScopeTree::new_root();
    let c1 = tree.create_child(&root(), "c1", OwnerLease::Generation(1)).unwrap();
    assert_eq!(c1, ScopePath(vec!["c1".into()]));
    // name-unique within the parent
    let err = tree.create_child(&root(), "c1", OwnerLease::Run(2)).unwrap_err();
    assert!(matches!(err, ScopeError::DuplicateScope { name, .. } if name == "c1"));
    // nested scopes are MVP non-goals (R-26)
    let err = tree.create_child(&c1, "c1-child", OwnerLease::Run(2)).unwrap_err();
    assert!(matches!(err, ScopeError::InvalidInput(_)));
    // disposal is recursive: disposing the root removes every child
    let mut registry =
        ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
    tree.dispose_scope(&root(), &mut registry, false).unwrap();
    assert!(tree.scopes().is_empty());

    // ephemeral: reopening rebuilds the tree from scratch — root only, and
    // the session layout holds no scope persistence artifact
    session.close().unwrap();
    let layout = session_dir_layout(&dir);
    assert!(
        layout.iter().all(|e| !e.contains("scope")),
        "session layout must not persist scopes: {layout:?}"
    );
    let session2 = Session::open(SessionConfig { dir: dir.to_path_buf(), ..Default::default() }).unwrap();
    let scopes = session2.scopes().scopes();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].path, root());
    assert!(scopes[0].children.is_empty());
    session2.close().unwrap();
}

// --- consistency 10: replay honesty — the boundary is reconstructable -------

#[test]
fn consistency_10_replay_honesty_explicit_boundary() {
    struct DropPlugin;
    impl PolicyPlugin for DropPlugin {
        fn decide(&self, _c: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::Drop { reason: "no-store".into() })
        }
        fn name(&self) -> &'static str {
            "drop-all"
        }
    }
    let dir = fresh_session_dir("m2-boundary");
    let _guard = DirGuard(dir.clone());
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        policy: Arc::new(DropPlugin),
        ..Default::default()
    })
    .unwrap();
    let adm = session
        .retain_candidate(Candidate {
            role: CandidateRole::UserInput,
            content: b"data".to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert!(matches!(adm, Admission::NonResumableBoundary { .. }));
    session.close().unwrap();

    // the boundary is a canonical fact: audit reconstruction accounts it
    // (opaque-but-inspectable, no upcaster) with no missing objects, and the
    // envelope payload names the reason + replay relevance
    let log = dir.join("log.zst");
    let (store, queue) = open_store(&dir);
    let report = reconstruct(&log, &Registry::new(), &store).unwrap();
    assert_eq!(report.kinds["retention_boundary"].count, 1);
    assert!(report.missing_objects.is_empty());
    assert!(report.upcast_errors.is_empty());
    shutdown_store(store, queue);
    let envs = envelopes(&log);
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].kind, "retention_boundary");
    assert_eq!(envs[0].payload["reason"], "no-store");
    assert_eq!(envs[0].payload["replay_relevant"], true);
    assert_eq!(envs[0].payload["kind"], "non_resumable");
}
