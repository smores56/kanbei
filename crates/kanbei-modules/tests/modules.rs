//! Integration tests for kanbei-modules against the built kanbei-guest wasm.
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from the
//! workspace root first; guest tests print `skip:` and pass without it. The
//! pure state/package tests (1, 3, 7, 8, 9) never need the guest.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kanbei_capabilities::{Capability, PolicyTemplate, TrustClass};
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::{Digest, Id128};
use kanbei_modules::{
    install_package, HeadFile, ModuleError, ModuleManager, ModuleOrigin, PackageError,
    PackageManifest, StateError, StateStore, StateUpdate,
};
use kanbei_objects::ObjectStore;
use kanbei_services::{ScopePath, ServiceDependency, ServiceKey, ServiceRegistry};
use kanbei_vm::{GuestError, Vm, VmConfig};

// --- helpers ---------------------------------------------------------------

fn tmp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-modules-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn cleanup(dir: PathBuf, queue: Arc<DurabilityQueue>) {
    let queue = Arc::try_unwrap(queue)
        .unwrap_or_else(|_| panic!("durability queue Arc still shared"));
    queue.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn no_epoch() -> VmConfig {
    // Non-fuel tests: unlimited fuel so the shim + host calls never trip the
    // default 1M budget; the epoch deadline is effectively off.
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

fn load_vm() -> Option<Vm> {
    match Vm::load(no_epoch()) {
        Ok(vm) => Some(vm),
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            None
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn manifest(id: Id128, source: &str, deps: Vec<ServiceDependency>) -> PackageManifest {
    PackageManifest {
        schema: 1,
        module_id: id,
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::User,
        scope: ScopePath(vec!["root".into()]),
        deps,
        capabilities: vec![],
        source: source.to_string(),
        state_schema: None,
    }
}

fn state_store(tag: &str) -> (PathBuf, StateStore, Arc<DurabilityQueue>) {
    let dir = tmp_dir(tag);
    let queue = Arc::new(DurabilityQueue::start(&format!("test-state-{tag}")));
    let state = StateStore::open(&dir, Arc::clone(&queue), Arc::new(|_| true));
    (dir, state, queue)
}

fn manager_setup(tag: &str, vm: Vm) -> (PathBuf, ModuleManager, Arc<DurabilityQueue>) {
    let dir = tmp_dir(tag);
    let queue = Arc::new(DurabilityQueue::start(&format!("test-modules-{tag}")));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let state = StateStore::open(&dir, Arc::clone(&queue), Arc::new(|_| true));
    let services = Arc::new(Mutex::new(ServiceRegistry::new()));
    let manager = ModuleManager::new(vm, store, state, services).unwrap();
    (dir, manager, queue)
}

fn root() -> ScopePath {
    ScopePath(vec!["root".into()])
}

fn svc_key(name: &str) -> ServiceKey {
    ServiceKey {
        scope: root(),
        name: name.into(),
    }
}

// --- guest sources (the M2 Luau contract: `kb_on_activate(ctx)` +
// `kb_hot`, top-level code pure) -------------------------------------------

const TRIVIAL_HOT: &str = r#"
function kb_on_activate(ctx) end
function kb_hot(x) return x * 2 end
"#;

const T2_ACTIVATE: &str = r#"
function kb_on_activate(ctx)
  ctx.log("activated")
  ctx.state_set("planner", 1, '{"attempts":0}')
end
function kb_hot(x) return x end
"#;

const T4_HOST_CALL: &str = r#"
function kb_on_activate(ctx) end
function kb_hot(x)
  return kb_host_call(1, '{"key":"planner"}')
end
"#;

const A_PUBLISH_S1: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"svc"}', 1, '[]')
end
function kb_hot(x)
  return kb_host_call(0, '{"msg":"a-hot"}')
end
"#;

const B_PUBLISH_S2: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"svc"}', 2, '[]')
end
function kb_hot(x) return "from-B" end
"#;

const C_USES_SVC: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"uses"}', 1, '[{"key":{"scope":["root"],"name":"svc"},"required_version":2}]')
end
function kb_hot(x) return x end
"#;

const A_SVC_RESPONDER: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"svc"}', 1, '[]')
end
function kb_hot(x) return { from = "A", got = x } end
"#;

const B_CALLS_SVC: &str = r#"
function kb_on_activate(ctx)
  local r = ctx.service_call('{"scope":["root"],"name":"svc"}', '{"n":42}')
  ctx.log(r)
end
function kb_hot(x) return x end
"#;

const T12_APPROVAL: &str = r#"
function kb_on_activate(ctx)
  local r = ctx.require_approval("process.run", '["start"]')
  ctx.log(r)
end
function kb_hot(x) return x end
"#;

// --- tests -----------------------------------------------------------------

#[test]
fn install_package_roundtrip_and_dedup() {
    let dir = tmp_dir("pkg");
    let queue = Arc::new(DurabilityQueue::start("test-modules-pkg"));
    let mut store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let m = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    let (d1, dedup1) = install_package(&mut store, &m).unwrap();
    assert!(!dedup1);
    let (d2, dedup2) = install_package(&mut store, &m).unwrap();
    assert_eq!(d1, d2);
    assert!(dedup2);
    assert_eq!(store.get(&d1).unwrap(), serde_json::to_vec(&m).unwrap());
    let back: PackageManifest = serde_json::from_slice(&store.get(&d1).unwrap()).unwrap();
    assert_eq!(back, m);
    // schema guard
    let bad = PackageManifest {
        schema: 2,
        ..m.clone()
    };
    let err = install_package(&mut store, &bad).unwrap_err();
    assert!(matches!(
        err,
        PackageError::SchemaMismatch {
            expected: 1,
            actual: 2
        }
    ));
    drop(store);
    cleanup(dir, queue);
}

#[test]
fn activate_runs_kb_on_activate_log_and_state() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("activate", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, T2_ACTIVATE, vec![])).unwrap();
    assert_eq!(g.generation, 1);
    assert_eq!(g.module_id, id);
    assert_eq!(manager.snapshot(), vec![(id, 1, g.package)]);
    // the activation entry's ctx.log reached the kernel log sink
    assert_eq!(manager.host().log_entries(), vec!["activated".to_string()]);
    // ctx.state_set created the head with seq 1
    let state = manager.state();
    let (head, bytes) = state.lock().unwrap().get("planner").unwrap().unwrap();
    assert_eq!(head.seq, 1);
    assert_eq!(head.schema, 1);
    assert_eq!(bytes, br#"{"attempts":0}"#);
    drop(state);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn state_cas_seq_digest_and_fail_closed() {
    let dir = tmp_dir("cas");
    let queue = Arc::new(DurabilityQueue::start("test-state-cas"));
    let mut state = StateStore::open(&dir, Arc::clone(&queue), Arc::new(|g| g == 1));
    state.set_max_state_bytes(64);
    let h1 = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":1}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    assert_eq!(h1.seq, 1);
    let h2 = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":2}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    assert_eq!(h2.seq, 2);
    assert_ne!(h1.digest, h2.digest);
    assert_eq!(h2.last_pinned, None);
    // oversized update: old head stays active
    let err = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: vec![b'x'; 100],
            generation: 1,
        })
        .unwrap_err();
    assert!(matches!(err, StateError::Oversized { bytes: 100, limit: 64, .. }));
    let (head, bytes) = state.get("k").unwrap().unwrap();
    assert_eq!(head.digest, h2.digest);
    assert_eq!(bytes, br#"{"a":2}"#);
    // schema mismatch: old head untouched
    let err = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 2,
            bytes: br#"{"a":3}"#.to_vec(),
            generation: 1,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        StateError::SchemaMismatch {
            expected: 1,
            actual: 2,
            ..
        }
    ));
    let (head, _) = state.get("k").unwrap().unwrap();
    assert_eq!(head.digest, h2.digest);
    // displaced generation: rejected
    let err = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":4}"#.to_vec(),
            generation: 2,
        })
        .unwrap_err();
    assert!(matches!(err, StateError::StaleGeneration { generation: 2 }));
    drop(state);
    cleanup(dir, queue);
}

#[test]
fn stale_generation_rejected() {
    // store-level currency gate
    let dir = tmp_dir("stale-store");
    let queue = Arc::new(DurabilityQueue::start("test-state-stale"));
    let mut state = StateStore::open(&dir, Arc::clone(&queue), Arc::new(|g| g == 1));
    let err = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: b"x".to_vec(),
            generation: 2,
        })
        .unwrap_err();
    assert!(matches!(err, StateError::StaleGeneration { generation: 2 }));
    drop(state);
    cleanup(dir, queue);

    // guest path: after deactivate the old token traps as StaleGeneration
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("stale-guest", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, T4_HOST_CALL, vec![])).unwrap();
    assert!(manager.generation_current(g.generation));
    assert_eq!(
        g.instance
            .lock()
            .unwrap()
            .call_json("kb_hot", "{}")
            .unwrap(),
        // kb_hot returns the host-call result as a Lua string, so call_json
        // JSON-encodes it once more.
        r#""{\"ok\":true,\"value\":null}""#
    );
    manager.deactivate(id).unwrap();
    assert!(!manager.generation_current(g.generation));
    let err = g
        .instance
        .lock()
        .unwrap()
        .call_json("kb_hot", "{}")
        .unwrap_err();
    assert!(matches!(err, GuestError::StaleGeneration), "got {err:?}");
    assert_eq!(manager.rejected_stale_effects(), 1);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn generation_replacement_rebinds_and_stales_old_token() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("replace", vm);
    let id = Id128::generate();
    let a = manager.activate(&manifest(id, A_PUBLISH_S1, vec![])).unwrap();
    // C publishes a service depending on svc v2 (version-compatible with B)
    manager.activate(&manifest(Id128::generate(), C_USES_SVC, vec![])).unwrap();
    let outcome = manager.replace(id, &manifest(id, B_PUBLISH_S2, vec![])).unwrap();
    assert_eq!(outcome.old.generation, a.generation);
    assert_eq!(outcome.new.generation, 3);
    assert_eq!(outcome.rebind, vec![svc_key("uses")]);
    assert!(outcome.restart.is_empty());
    // S resolves to B's generation
    let provider = manager
        .services()
        .lock()
        .unwrap()
        .resolve(&svc_key("svc"), 2, &root())
        .unwrap()
        .clone();
    assert_eq!(provider.module_id, id);
    assert_eq!(provider.generation, outcome.new.generation);
    // the old generation's token is stale
    let err = a
        .instance
        .lock()
        .unwrap()
        .call_json("kb_hot", "{}")
        .unwrap_err();
    assert!(matches!(err, GuestError::StaleGeneration), "got {err:?}");
    // deactivating B with the dependent C still attached fails without
    // mutating anything
    let err = manager.deactivate(id).unwrap_err();
    match err {
        ModuleError::DependentsRemain {
            module_id,
            dependents,
        } => {
            assert_eq!(module_id, id);
            assert_eq!(dependents, vec![ServiceDependency {
                key: svc_key("uses"),
                required_version: 2,
            }]);
        }
        other => panic!("expected DependentsRemain, got {other:?}"),
    }
    assert!(manager.generation_current(outcome.new.generation));
    drop(a);
    drop(outcome);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn service_call_routes_to_provider_kb_hot() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("svc-call", vm);
    let id_a = Id128::generate();
    manager.activate(&manifest(id_a, A_SVC_RESPONDER, vec![])).unwrap();
    let dep = ServiceDependency {
        key: svc_key("svc"),
        required_version: 1,
    };
    let id_b = Id128::generate();
    manager.activate(&manifest(id_b, B_CALLS_SVC, vec![dep])).unwrap();
    // B's activation entry called svc through the dispatcher; the response is
    // A's kb_hot result JSON, logged by B
    let log = manager.host().log_entries();
    assert_eq!(log.len(), 1);
    assert!(log[0].contains(r#""from":"A""#), "log entry: {}", log[0]);
    assert!(log[0].contains(r#""n":42"#), "log entry: {}", log[0]);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn corrupt_head_detected() {
    let (dir, mut state, queue) = state_store("corrupt");
    let h = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":1}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    let head_path = dir.join("state").join("k.head");
    // valid JSON with a tampered checksum
    let bad = HeadFile {
        checksum: Digest::new(b"tampered"),
        ..h
    };
    std::fs::write(&head_path, bad.to_bytes()).unwrap();
    let err = state.get("k").unwrap_err();
    assert!(matches!(err, StateError::CorruptHead { key, .. } if key == "k"));
    // non-JSON head file is corrupt too
    std::fs::write(&head_path, b"not json at all").unwrap();
    assert!(matches!(state.get("k"), Err(StateError::CorruptHead { .. })));
    drop(state);
    cleanup(dir, queue);
}

#[test]
fn mark_pinned_and_prune_unpinned() {
    let (dir, mut state, queue) = state_store("prune");
    let d1 = state
        .cas(StateUpdate {
            key: "k1".into(),
            schema: 1,
            bytes: br#"{"v":1}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    let d1b = state
        .cas(StateUpdate {
            key: "k1".into(),
            schema: 1,
            bytes: br#"{"v":2}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    assert_ne!(d1b, d1);
    let d2 = state
        .cas(StateUpdate {
            key: "k2".into(),
            schema: 1,
            bytes: br#"{"v":3}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    state.mark_pinned("k1", d1).unwrap();
    let d1c = state
        .cas(StateUpdate {
            key: "k1".into(),
            schema: 1,
            bytes: br#"{"v":4}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    let (head, _) = state.get("k1").unwrap().unwrap();
    assert_eq!(head.digest, d1c);
    assert_eq!(head.last_pinned, Some(d1));
    assert_eq!(head.seq, 4);
    // objects on disk: {d1, d1b, d1c, d2}; heads: {d1c, d2}; `referenced`
    // = execution-snapshot pins (d1) — d1b is the only private unreferenced
    // snapshot.
    let referenced = HashSet::from([d1]);
    assert_eq!(state.prune_unpinned(&referenced, 0).unwrap(), 1);
    assert_eq!(state.prune_unpinned(&referenced, 0).unwrap(), 0);
    // dropping the pin exposes d1; current-head snapshots stay protected
    assert_eq!(state.prune_unpinned(&HashSet::new(), 0).unwrap(), 1);
    assert_eq!(state.prune_unpinned(&HashSet::new(), 0).unwrap(), 0);
    // heads survive every prune
    let (h1, _) = state.get("k1").unwrap().unwrap();
    let (h2, _) = state.get("k2").unwrap().unwrap();
    assert_eq!(h1.digest, d1c);
    assert_eq!(h2.digest, d2);
    assert_eq!(state.heads().unwrap().len(), 2);
    // grace keeps the digest-sorted tail of the candidates
    let d1d = state
        .cas(StateUpdate {
            key: "k1".into(),
            schema: 1,
            bytes: br#"{"v":5}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    let d2b = state
        .cas(StateUpdate {
            key: "k2".into(),
            schema: 1,
            bytes: br#"{"v":6}"#.to_vec(),
            generation: 1,
        })
        .unwrap()
        .digest;
    assert_eq!(state.prune_unpinned(&HashSet::new(), 1).unwrap(), 1);
    assert_eq!(state.prune_unpinned(&HashSet::new(), 1).unwrap(), 0);
    assert_eq!(state.prune_unpinned(&HashSet::new(), 0).unwrap(), 1);
    // the two current heads (d1d, d2b) were never candidates
    assert_eq!(state.get("k1").unwrap().unwrap().0.digest, d1d);
    assert_eq!(state.get("k2").unwrap().unwrap().0.digest, d2b);
    drop(state);
    cleanup(dir, queue);
}

#[test]
fn state_overflow_rejected_atomically() {
    let (dir, mut state, queue) = state_store("overflow");
    state.set_max_state_bytes(8);
    state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: b"tiny".to_vec(),
            generation: 1,
        })
        .unwrap();
    let err = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: vec![b'x'; 100],
            generation: 1,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        StateError::Oversized {
            key,
            bytes: 100,
            limit: 8
        } if key == "k"
    ));
    let (head, bytes) = state.get("k").unwrap().unwrap();
    assert_eq!(head.seq, 1);
    assert_eq!(bytes, b"tiny");
    drop(state);
    cleanup(dir, queue);
}

#[test]
fn syntax_error_activation_fails_nothing_registered() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("syntax", vm);
    let id = Id128::generate();
    let err = manager
        .activate(&manifest(id, "local x = = 1", vec![]))
        .unwrap_err();
    assert!(matches!(err, ModuleError::Vm(GuestError::Compile(_))), "got {err:?}");
    assert!(manager.snapshot().is_empty());
    assert!(manager.host().log_entries().is_empty());
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn disposal_record_and_vm_containment() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("dispose", vm);
    let id = Id128::generate();
    let a = manager.activate(&manifest(id, TRIVIAL_HOT, vec![])).unwrap();
    let rec = a.dispose();
    assert_eq!(rec.generation, 1);
    assert!(!rec.forced);
    // the vm is unaffected: a fresh activation + call succeed
    let id2 = Id128::generate();
    let b = manager.activate(&manifest(id2, TRIVIAL_HOT, vec![])).unwrap();
    assert_eq!(
        b.instance.lock().unwrap().call_json("kb_hot", "5").unwrap(),
        "10"
    );
    let rec2 = manager.deactivate(id2).unwrap();
    assert_eq!(rec2.generation, 2);
    assert!(!rec2.forced);
    assert!(manager.snapshot().is_empty());
    // deactivating a never-activated module is a typed error
    let err = manager.deactivate(Id128::generate()).unwrap_err();
    assert!(matches!(err, ModuleError::NotActivated { .. }));
    drop(b);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn require_approval_returns_intent_shape() {
    let Some(vm) = load_vm() else { return };
    let (dir, mut manager, queue) = manager_setup("approval", vm);
    manager
        .host()
        .broker()
        .lock()
        .unwrap()
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Agent,
            allow: vec![],
            deny: vec![],
            require_approval: vec![Capability::new("process.run".into(), vec!["start".into()])],
            monotonic: true,
            version: 1,
        })
        .unwrap();
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, T12_APPROVAL, vec![])).unwrap();
    let log = manager.host().log_entries();
    assert_eq!(log.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&log[0]).unwrap();
    let intent = &v["intent"];
    assert_eq!(intent["action"], "process.run");
    assert_eq!(intent["scope"], "run");
    assert_eq!(intent["module_generation"], g.generation);
    assert_eq!(intent["principal"]["generation"], g.generation);
    assert!(intent["digest"].as_str().unwrap().starts_with("blake3:"));
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}
