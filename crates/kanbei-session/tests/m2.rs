//! Integration tests for kanbei-session M2 wiring: transactional config
//! reload (activate_config), generation replacement, effect dispatch,
//! module-state head updates, the retention gate, safe mode, trap
//! containment, M2 fault points, and schema-2 manifest pinning.
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from the
//! workspace root first; module-dependent tests print `skip:` and pass without
//! it (the suite must stay green either way).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kanbei_capabilities::TrustClass;
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_log::for_each_frame;
use kanbei_modules::{ModuleError, ModuleOrigin, PackageManifest};
use kanbei_policy::builtins::PatternRedactionPolicy;
use kanbei_policy::{
    Admission, Candidate, CandidateRole, PolicyError, PolicyPlugin, RetentionDecision,
};
use kanbei_services::{
    ScopePath, ServiceContract, ServiceDependency, ServiceError, ServiceKey, ServiceProvider,
};
use kanbei_session::{FaultInjector, FaultPoint, NewEvent, Session, SessionConfig, SessionError};
use kanbei_snapshot::ExecutionManifest;
use kanbei_vm::{GuestError, Vm, VmConfig};
use serde_json::{Value, json};

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-m2-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
/// trap (the trap test uses the default fuel budget instead).
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
    PackageManifest {
        schema: 1,
        module_id: id,
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::User,
        scope: root(),
        deps,
        capabilities: vec![],
        source: source.to_string(),
        state_schema: None,
    }
}

fn open(dir: &Path) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .unwrap()
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
fn envelopes(log_path: &Path) -> Vec<Envelope> {
    let mut out = Vec::new();
    for_each_frame(log_path, |frame| {
        for line in &frame.events {
            out.push(Envelope::from_line(line).unwrap());
        }
    })
    .unwrap();
    out
}

/// Fault injector that records every point it sees, in order.
struct Recorder(Arc<Mutex<Vec<FaultPoint>>>);

impl Recorder {
    fn new() -> (Self, Arc<Mutex<Vec<FaultPoint>>>) {
        let points = Arc::new(Mutex::new(Vec::new()));
        (Self(Arc::clone(&points)), points)
    }
}

impl FaultInjector for Recorder {
    fn inject(&self, point: FaultPoint) {
        self.0.lock().unwrap().push(point);
    }
}

// --- guest sources (the M2 Luau contract: `kb_on_activate(ctx)` +
// `kb_hot`, top-level code pure) -------------------------------------------

const TRIVIAL: &str = r#"
function kb_on_activate(ctx) end
function kb_hot(x) return x end
"#;

/// Publishes `greeter` v1 and answers kb_hot with its identity.
const PUBLISHER: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greeter"}', 1, '[]')
end
function kb_hot(x) return { from = "greeter", got = x } end
"#;

/// Publishes `greeter` v2 (generation replacement).
const REPLACER: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greeter"}', 2, '[]')
end
function kb_hot(x) return { from = "replacer", got = x } end
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

// --- tests -----------------------------------------------------------------

/// (a) activate_config success: the module's kb_on_activate service lands in
/// the shared registry, the composition epoch bumps, the canonical
/// composition_changed event is on the log, and the pinned manifest carries
/// the module pin + composition digest (schema 2).
#[test]
fn activate_config_publishes_service_and_composition() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("activate");
    let id = Id128::generate();
    let m = manifest(id, PUBLISHER, vec![]);
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(m.clone()),
        ..Default::default()
    })
    .unwrap();
    // composition epoch bumped; the contribution set holds the service
    assert_eq!(session.composition().epoch, 1);
    assert_eq!(session.composition().contributions.len(), 1);
    // the service is resolvable through the shared registry
    let provider = session
        .modules()
        .unwrap()
        .services()
        .lock()
        .unwrap()
        .resolve(&svc_key("greeter"), 1, &root())
        .unwrap()
        .clone();
    assert_eq!(provider.module_id, id);
    assert_eq!(provider.generation, 1);
    // the composition's canonical bytes are pinned as an object (the epoch
    // digest ref is closure-valid)
    assert!(session.store().exists(&session.composition().digest));
    // the post-event manifest carries the module pin + composition (schema 2)
    let manifest_digest = session
        .current_snapshot()
        .expect("state change pins a manifest");
    let bytes = session.store().get(&manifest_digest).unwrap();
    let manifest: ExecutionManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest.schema, 4);
    assert_eq!(manifest.module_abi, Some(1));
    assert_eq!(manifest.modules.len(), 1);
    assert_eq!(manifest.modules[0].module_id, id);
    assert_eq!(manifest.modules[0].generation, 1);
    assert_eq!(manifest.modules[0].scope, "/");
    assert_eq!(manifest.composition, Some(session.composition().digest));
    assert_eq!(manifest.state_head, Some(session.composition().digest));
    assert!(manifest.engine_digest.is_some());
    assert_eq!(manifest.toolchain_digest, None);
    // the log holds exactly one event: composition_changed with the epoch
    // delta {added: [module], removed: []}, scope "/", initiator "config"
    session.close().unwrap();
    let recovered = kanbei_log::recover(&dir.path().join("log.zst")).unwrap();
    assert_eq!(recovered.events, 1);
    let envs = envelopes(&dir.path().join("log.zst"));
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].kind, "composition_changed");
    assert_eq!(envs[0].payload["epoch"], 1);
    assert_eq!(
        envs[0].payload["delta"]["added"][0]["module_id"],
        id.to_string()
    );
    assert_eq!(envs[0].payload["delta"]["added"][0]["generation"], 1);
    assert_eq!(envs[0].payload["delta"]["removed"], json!([]));
    assert_eq!(envs[0].payload["scope"], "/");
    assert_eq!(envs[0].payload["initiator"], "config");
}

/// (b) activate_config failure on a service conflict: Err, epoch unchanged,
/// no composition_changed event, the failing module never registers.
#[test]
fn activate_config_conflict_retains_epoch() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("conflict");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    // pre-publish a conflicting provider for the key the config module wants
    let holder = ServiceProvider {
        module_id: Id128::generate(),
        generation: 999,
        contract: ServiceContract {
            name: "greeter".into(),
            version: 1,
        },
    };
    session
        .modules()
        .unwrap()
        .services()
        .lock()
        .unwrap()
        .publish(svc_key("greeter"), holder)
        .unwrap();
    let id = Id128::generate();
    let err = session
        .activate_config(manifest(id, PUBLISHER, vec![]))
        .unwrap_err();
    assert!(
        matches!(err, SessionError::Module(ModuleError::Activation(_))),
        "got {err:?}"
    );
    // atomic publish: epoch untouched, nothing on the log
    assert_eq!(session.composition().epoch, 0);
    let recovered = kanbei_log::recover(&dir.path().join("log.zst")).unwrap();
    assert_eq!(recovered.events, 0);
    assert!(session.modules().unwrap().snapshot().is_empty());
    session.close().unwrap();
}

/// (c) replace_module: the old generation's service is gone, the new
/// generation's is present, stale tokens are rejected, and the
/// composition_changed delta records removed + added.
#[test]
fn replace_module_swaps_generation_and_records_delta() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("replace");
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id, PUBLISHER, vec![])),
        ..Default::default()
    })
    .unwrap();
    let outcome = session
        .replace_module(id, manifest(id, REPLACER, vec![]))
        .unwrap();
    assert_eq!(outcome.old.generation, 1);
    assert_eq!(outcome.new.generation, 2);
    assert!(outcome.rebind.is_empty());
    // the old contract version is gone, the new one resolves
    let reg = session.modules().unwrap().services();
    let provider = reg
        .lock()
        .unwrap()
        .resolve(&svc_key("greeter"), 2, &root())
        .unwrap()
        .clone();
    assert_eq!(provider.module_id, id);
    assert_eq!(provider.generation, 2);
    assert!(matches!(
        reg.lock().unwrap().resolve(&svc_key("greeter"), 1, &root()),
        Err(ServiceError::VersionMismatch { .. })
    ));
    // composition epoch bumped; the delta records removed (old) + added (new)
    assert_eq!(session.composition().epoch, 2);
    let envs = envelopes(&dir.path().join("log.zst"));
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[1].kind, "composition_changed");
    assert_eq!(envs[1].payload["delta"]["added"][0]["generation"], 2);
    assert_eq!(envs[1].payload["delta"]["removed"][0]["generation"], 1);
    // the stale caller generation cannot dispatch effects
    let err = session
        .effect_dispatch(&svc_key("greeter"), "{}", 1)
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::StaleGeneration { generation: 1 }
    ));
    // drop the generation handle before close: a live Generation keeps the
    // instance (and through it the host's state store) alive
    drop(outcome);
    session.close().unwrap();
}

/// (d) effect_dispatch routes through the host's service_call machinery to
/// the provider generation's kb_hot; stale caller generations are rejected.
#[test]
fn effect_dispatch_routes_to_provider_kb_hot() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("dispatch");
    let id_prov = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id_prov, PUBLISHER, vec![])),
        ..Default::default()
    })
    .unwrap();
    let dep = ServiceDependency {
        key: svc_key("greeter"),
        required_version: 1,
    };
    let id_caller = Id128::generate();
    let caller = session
        .activate_config(manifest(id_caller, CALLER, vec![dep]))
        .unwrap();
    assert_eq!(caller.epoch, 2);
    let result = session
        .effect_dispatch(&svc_key("greeter"), r#"{"n":42}"#, caller.generation)
        .unwrap();
    let v: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(v["from"], "greeter");
    assert_eq!(v["got"]["n"], 42);
    // a stale caller generation is rejected before any host work
    let err = session
        .effect_dispatch(&svc_key("greeter"), "{}", 4242)
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::StaleGeneration { generation: 4242 }
    ));
    session.close().unwrap();
}

/// (e) module_state_cas: head seq bumps through the session actor; stale
/// generations and oversize updates fail closed.
#[test]
fn module_state_cas_heads_and_fail_closed() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("head");
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        max_state_bytes: 64,
        config: Some(manifest(id, TRIVIAL, vec![])),
        ..Default::default()
    })
    .unwrap();
    let generation = session.modules().unwrap().snapshot()[0].1;
    let h1 = session
        .module_state_cas("k", 1, br#"{"a":1}"#.to_vec(), generation)
        .unwrap();
    assert_eq!(h1.seq, 1);
    let h2 = session
        .module_state_cas("k", 1, br#"{"a":2}"#.to_vec(), generation)
        .unwrap();
    assert_eq!(h2.seq, 2);
    assert_ne!(h1.digest, h2.digest);
    // stale generation
    let err = session
        .module_state_cas("k", 1, b"x".to_vec(), 999)
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::State(kanbei_modules::StateError::StaleGeneration { generation: 999 })
    ));
    // oversize: old head stays active
    let err = session
        .module_state_cas("k", 1, vec![b'x'; 100], generation)
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::State(kanbei_modules::StateError::Oversized {
            bytes: 100,
            limit: 64,
            ..
        })
    ));
    let state = session.modules().unwrap().state();
    let (head, bytes) = state.lock().unwrap().get("k").unwrap().unwrap();
    assert_eq!(head.digest, h2.digest);
    assert_eq!(bytes, br#"{"a":2}"#);
    drop(state);
    session.close().unwrap();
}

/// (f) retain_candidate: StoreAll stores unchanged; the candidate never
/// touches the log or the object store.
#[test]
fn retain_candidate_store_all_never_touches_storage() {
    let dir = TempDir::new("retain-store");
    let mut session = open(dir.path());
    let adm = session
        .retain_candidate(Candidate {
            role: CandidateRole::ModelContext,
            content: b"hello world".to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert_eq!(
        adm,
        Admission::Stored {
            bytes: b"hello world".to_vec()
        }
    );
    // the candidate never reached storage: the log is empty and the store
    // holds only the genesis manifest
    let recovered = kanbei_log::recover(&dir.path().join("log.zst")).unwrap();
    assert_eq!(recovered.events, 0);
    assert_eq!(session.store().scan().unwrap().len(), 1);
    session.close().unwrap();
}

/// (f) retain_candidate with the pattern-redaction policy: matched content is
/// transformed before storage.
#[test]
fn retain_candidate_redaction_transforms() {
    let dir = TempDir::new("retain-redact");
    let policy = Arc::new(
        PatternRedactionPolicy::new(vec!["SECRET-\\d+".into()], "[redacted]".into()).unwrap(),
    );
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        policy,
        ..Default::default()
    })
    .unwrap();
    let adm = session
        .retain_candidate(Candidate {
            role: CandidateRole::ToolOutput,
            content: b"token SECRET-1234 end".to_vec(),
            replay_relevant: false,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    match adm {
        Admission::Stored { bytes } => assert_eq!(bytes, b"token [redacted] end"),
        other => panic!("expected Stored with transformed bytes, got {other:?}"),
    }
    session.close().unwrap();
}

/// (f) retain_candidate on a replay-relevant drop: the gate returns the
/// non-resumable boundary and the session commits the canonical
/// `retention_boundary` fact.
#[test]
fn retain_candidate_drop_boundary_commits_fact() {
    let dir = TempDir::new("retain-boundary");
    struct DropPlugin;
    impl PolicyPlugin for DropPlugin {
        fn decide(&self, _c: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::Drop {
                reason: "no-store".into(),
            })
        }
        fn name(&self) -> &'static str {
            "drop-all"
        }
    }
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
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
    let envs = envelopes(&dir.path().join("log.zst"));
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].kind, "retention_boundary");
    assert_eq!(envs[0].payload["reason"], "no-store");
    assert_eq!(envs[0].payload["replay_relevant"], true);
    assert_eq!(envs[0].payload["kind"], "non_resumable");
    // a rejection commits the same fact with kind "rejected"
    struct RejectPlugin;
    impl PolicyPlugin for RejectPlugin {
        fn decide(&self, _c: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::RejectExecution {
                reason: "denied".into(),
            })
        }
        fn name(&self) -> &'static str {
            "reject-all"
        }
    }
    let dir2 = TempDir::new("retain-reject");
    let mut session2 = Session::open(SessionConfig {
        dir: dir2.path().to_path_buf(),
        policy: Arc::new(RejectPlugin),
        ..Default::default()
    })
    .unwrap();
    let adm = session2
        .retain_candidate(Candidate {
            role: CandidateRole::ModelContext,
            content: b"x".to_vec(),
            replay_relevant: false,
            sensitivity: None,
            media: None,
        })
        .unwrap();
    assert!(matches!(adm, Admission::Rejected { .. }));
    let envs = envelopes(&dir2.path().join("log.zst"));
    assert_eq!(envs[0].kind, "retention_boundary");
    assert_eq!(envs[0].payload["kind"], "rejected");
    session2.close().unwrap();
    session.close().unwrap();
}

/// (g) safe mode: a config manifest that fails activation opens the session
/// with modules dropped and a canonical `safe_mode_activated` event on the
/// log; the session remains usable with storage only (R-01/C-02).
#[test]
fn invalid_config_opens_safe_mode() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("safe-mode");
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id, "local x = = 1", vec![])),
        ..Default::default()
    })
    .unwrap();
    assert!(session.modules().is_none());
    assert_eq!(session.vm_engine_digest(), None);
    // the session remains usable with storage only
    let receipt = session
        .commit(vec![event("post-safe", json!({"n": 1}))], None)
        .unwrap();
    assert_eq!(receipt.first_seq, 2);
    let envs = envelopes(&dir.path().join("log.zst"));
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[0].kind, "safe_mode_activated");
    assert!(
        envs[0].payload["reason"]
            .as_str()
            .unwrap()
            .contains("compile")
    );
    session.close().unwrap();
}

/// (h) trap containment: a module whose kb_hot burns its fuel budget fails
/// the dispatch with a typed error; the session actor keeps committing and
/// recovery stays clean.
#[test]
fn wasm_trap_contained_session_survives() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("trap");
    let id_trap = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        // 50M fuel: enough for the activation shim (which alone needs >1M),
        // while kb_hot's loop deterministically exhausts the per-call budget
        // (no epoch deadline, so the trap is fuel, not a watchdog tick)
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
    let caller = session
        .activate_config(manifest(id_caller, CALLER, vec![dep]))
        .unwrap();
    let err = session
        .effect_dispatch(&svc_key("trap"), "{}", caller.generation)
        .unwrap_err();
    assert!(matches!(err, SessionError::Effect(_)), "got {err:?}");
    // the session actor still commits and recovers
    let receipt = session
        .commit(vec![event("post-trap", json!({"n": 1}))], None)
        .unwrap();
    assert_eq!(receipt.first_seq, 3);
    session.close().unwrap();
    let recovered = kanbei_log::recover(&dir.path().join("log.zst")).unwrap();
    assert_eq!(recovered.events, 3);
    let session2 = open(dir.path());
    assert_eq!(session2.next_seq(), 4);
    session2.close().unwrap();
}

/// (i) the M2 fault points record around activate_config, effect_dispatch,
/// and module_state_cas.
#[test]
fn m2_fault_points_recorded() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("fault-points");
    let (recorder, points) = Recorder::new();
    let id_prov = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        fault: Some(Arc::new(recorder)),
        config: Some(manifest(id_prov, PUBLISHER, vec![])),
        ..Default::default()
    })
    .unwrap();
    let dep = ServiceDependency {
        key: svc_key("greeter"),
        required_version: 1,
    };
    let id_caller = Id128::generate();
    let caller = session
        .activate_config(manifest(id_caller, CALLER, vec![dep]))
        .unwrap();
    session
        .effect_dispatch(&svc_key("greeter"), "{}", caller.generation)
        .unwrap();
    session
        .module_state_cas("k", 1, br#"{}"#.to_vec(), caller.generation)
        .unwrap();
    let got = points.lock().unwrap().clone();
    let m2: Vec<FaultPoint> = got
        .iter()
        .copied()
        .filter(|p| {
            matches!(
                p,
                FaultPoint::BeforeConfigActivation
                    | FaultPoint::AfterConfigActivation
                    | FaultPoint::BeforeEffectDispatch
                    | FaultPoint::AfterEffectDispatch
                    | FaultPoint::BeforeHeadUpdate
                    | FaultPoint::AfterHeadUpdate
            )
        })
        .collect();
    assert_eq!(
        m2,
        vec![
            FaultPoint::BeforeConfigActivation, // open: config module
            FaultPoint::AfterConfigActivation,
            FaultPoint::BeforeConfigActivation, // caller activation
            FaultPoint::AfterConfigActivation,
            FaultPoint::BeforeEffectDispatch,
            FaultPoint::AfterEffectDispatch,
            FaultPoint::BeforeHeadUpdate,
            FaultPoint::AfterHeadUpdate,
        ]
    );
    session.close().unwrap();
}

/// (j) every manifest pinned by M2 state changes parses as schema 2 with the
/// module pins and the composition digest.
#[test]
fn committed_manifests_are_schema_2() {
    if !require_guest() {
        return;
    };
    let dir = TempDir::new("schema2");
    let id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config: Some(manifest(id, PUBLISHER, vec![])),
        ..Default::default()
    })
    .unwrap();
    session
        .replace_module(id, manifest(id, REPLACER, vec![]))
        .unwrap();
    // a plain state-changing commit pins a manifest too
    session
        .commit(
            vec![event("change", json!({"s": 1}))],
            Some(Digest::new(b"head")),
        )
        .unwrap();
    let mut manifests = 0;
    for digest in session.store().scan().unwrap() {
        let bytes = session.store().get(&digest).unwrap();
        let Ok(m) = serde_json::from_slice::<ExecutionManifest>(&bytes) else {
            continue; // packages, compositions, state snapshots — not manifests
        };
        manifests += 1;
        assert_eq!(m.schema, 4, "manifest {digest} must be schema 4");
        assert_eq!(m.module_abi, Some(1));
        // genesis (bootstrap) carries no pins; every later manifest pins the
        // single active module and a composition digest
        if m.modules.is_empty() {
            assert_eq!(m.composition, None, "genesis must not pin a composition");
            continue;
        }
        assert_eq!(m.modules.len(), 1);
        assert_eq!(m.modules[0].module_id, id);
        assert_eq!(m.modules[0].scope, "/");
        // each manifest pins the composition digest current at its commit;
        // only the latest equals the live composition
        assert!(m.composition.is_some());
        assert!(m.engine_digest.is_some());
        assert_eq!(m.toolchain_digest, None);
    }
    // genesis (schema 2) + activation manifest + replacement manifest +
    // state-change manifest
    assert_eq!(manifests, 4);
    // the latest manifest pins the live composition digest and the current
    // module generation (the replacement)
    let latest: ExecutionManifest = serde_json::from_slice(
        &session
            .store()
            .get(&session.current_snapshot().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(latest.composition, Some(session.composition().digest));
    assert_eq!(latest.modules[0].generation, 2);
    session.close().unwrap();
}
