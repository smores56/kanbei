//! Integration tests for kanbei-modules against the built kanbei-guest wasm.
//!
//! A missing guest is a hard failure: build it with `cargo xtask build-guest`
//! from the workspace root first. The pure state/package tests (1, 3, 7, 8, 9)
//! never need the guest.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanbei_capabilities::{Capability, PolicyTemplate, TrustClass};
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::{Digest, Id128};
use kanbei_modules::{
    install_package, ActorError, HeadFile, HookError, ModuleError, ModuleManager, ModuleOrigin,
    PackageError, PackageManifest, StateError, StateStore, StateUpdate, HOOK_WAIT,
};
use kanbei_objects::ObjectStore;
use kanbei_scopes::contrib::{ContributionKind, HookKind};
use kanbei_services::{ScopePath, ServiceDependency, ServiceKey, ServiceRegistry};
use kanbei_vm::{GuestError, Host, Vm, VmConfig};

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

fn load_vm() -> Vm {
    match Vm::load(no_epoch()) {
        Ok(vm) => vm,
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn manifest(id: Id128, source: &str, deps: Vec<ServiceDependency>) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: id,
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::User,
        scope: ScopePath(vec!["root".into()]),
        deps,
        capabilities: vec![],
        source: source.to_string(),
        state_schema: None,
        state_key: None,
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

// T20: a mutual pair — each publishes a service and, when hot-called, calls the
// other's. The scope cycle rule must reject A→B→A before the second hop.
const A_CALLS_SVC_B: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"a"}', 1, '[]')
end
function kb_hot(x)
  return kb_host_call(3, '{"key":{"scope":["root"],"name":"b"},"args":{}}')
end
"#;

const B_CALLS_SVC_A: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"b"}', 1, '[]')
end
function kb_hot(x)
  return kb_host_call(3, '{"key":{"scope":["root"],"name":"a"},"args":{}}')
end
"#;

// T20: a non-cyclic multi-hop chain A→B→C. B forwards to C; C is the leaf.
const B_FORWARDS_TO_SVC_C: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"b"}', 1, '[]')
end
function kb_hot(x)
  return kb_host_call(3, '{"key":{"scope":["root"],"name":"c"},"args":{}}')
end
"#;

const C_RETURNS_HOP: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"c"}', 1, '[]')
end
function kb_hot(x) return { hop = 3 } end
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
        schema: kanbei_modules::PACKAGE_SCHEMA + 1,
        ..m.clone()
    };
    let err = install_package(&mut store, &bad).unwrap_err();
    assert!(matches!(
        err,
        PackageError::SchemaMismatch {
            expected,
            actual
        } if expected == kanbei_modules::PACKAGE_SCHEMA
            && actual == kanbei_modules::PACKAGE_SCHEMA + 1
    ));
    drop(store);
    cleanup(dir, queue);
}

#[test]
fn activate_runs_kb_on_activate_log_and_state() {
    let vm = load_vm();
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

    // guest path: after deactivate the generation's actor is drained and gone,
    // so the retired generation cannot act at all (a stronger guarantee than a
    // stale token trap; the token-level fence is covered by host.rs unit tests)
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("stale-guest", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, T4_HOST_CALL, vec![])).unwrap();
    assert!(manager.generation_current(g.generation));
    let runtime = Arc::clone(&g.runtime);
    assert_eq!(
        runtime.hot("kb_hot", "{}").unwrap().unwrap(),
        // kb_hot returns the host-call result as a Lua string, so call_json
        // JSON-encodes it once more.
        r#""{\"ok\":true,\"value\":null}""#
    );
    manager.deactivate(id).unwrap();
    assert!(!manager.generation_current(g.generation));
    assert!(matches!(runtime.hot("kb_hot", "{}"), Err(ActorError::Gone)));
    assert_eq!(manager.leaked_threads(), 0);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn generation_replacement_rebinds_and_stales_old_token() {
    let vm = load_vm();
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
    // the old generation's actor was drained by the replacement
    assert!(matches!(
        a.runtime.hot("kb_hot", "{}"),
        Err(ActorError::Gone)
    ));
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

/// F: `force_deactivate` tears a generation down even when its services still
/// have dependents — the safe-mode drop path, where the committed removal must
/// be TRUE rather than a best-effort no-op.
#[test]
fn force_deactivate_removes_a_generation_with_dependents() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("force-deactivate", vm);
    let id = Id128::generate();
    manager.activate(&manifest(id, A_PUBLISH_S1, vec![])).unwrap();
    // C publishes a service depending on svc v1.
    manager.activate(&manifest(Id128::generate(), C_USES_SVC, vec![])).unwrap();
    // Normal deactivate refuses while the dependent is attached...
    assert!(matches!(
        manager.deactivate(id),
        Err(ModuleError::DependentsRemain { .. })
    ));
    // ...but force_deactivate removes the generation and its service.
    manager.force_deactivate(id).unwrap();
    assert!(
        manager
            .services()
            .lock()
            .unwrap()
            .resolve(&svc_key("svc"), 1, &root())
            .is_err(),
        "the forced teardown unpublished the generation's service"
    );
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn service_call_routes_to_provider_kb_hot() {
    let vm = load_vm();
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

/// T20: a mutual service-call chain (A→B→A) is rejected by the scope cycle
/// rule before the second hop, so neither actor wedges waiting on the other.
#[test]
fn mutually_recursive_service_call_is_rejected_not_deadlocked() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("svc-cycle", vm);
    let dep_a = ServiceDependency {
        key: svc_key("a"),
        required_version: 1,
    };
    let dep_b = ServiceDependency {
        key: svc_key("b"),
        required_version: 1,
    };
    let a = manager
        .activate(&manifest(Id128::generate(), A_CALLS_SVC_B, vec![dep_b]))
        .unwrap();
    manager
        .activate(&manifest(Id128::generate(), B_CALLS_SVC_A, vec![dep_a]))
        .unwrap();
    let err = manager
        .call_generation(a.generation, "{}")
        .expect_err("A→B→A must be rejected");
    let msg = format!("{err:?}");
    assert!(msg.contains("already on the call chain"), "{msg}");
    drop(a);
    drop(manager);
    cleanup(dir, queue);
}

/// T20: a non-cyclic multi-hop chain (A→B→C) routes through every generation's
/// actor and the deepest result propagates back to the root caller. Pins that
/// the scope rides the mailbox and a legitimate 2-hop chain is not rejected.
#[test]
fn multi_hop_service_call_propagates_the_deepest_result() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("svc-3hop", vm);
    let dep_b = ServiceDependency {
        key: svc_key("b"),
        required_version: 1,
    };
    let dep_c = ServiceDependency {
        key: svc_key("c"),
        required_version: 1,
    };
    manager
        .activate(&manifest(Id128::generate(), C_RETURNS_HOP, vec![]))
        .unwrap();
    manager
        .activate(&manifest(
            Id128::generate(),
            B_FORWARDS_TO_SVC_C,
            vec![dep_c],
        ))
        .unwrap();
    let a = manager
        .activate(&manifest(Id128::generate(), A_CALLS_SVC_B, vec![dep_b]))
        .unwrap();
    let out = manager.call_generation(a.generation, "{}").unwrap();
    assert!(
        out.contains("hop") && out.contains('3'),
        "deepest result must propagate to the root caller: {out}"
    );
    drop(a);
    drop(manager);
    cleanup(dir, queue);
}

#[test]
fn exhausted_generation_budget_fails_activation() {
    let vm = match Vm::load(VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        generation_budget: Duration::ZERO,
        ..Default::default()
    }) {
        Ok(vm) => vm,
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest`")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    };
    let (dir, mut manager, queue) = manager_setup("budget", vm);
    let err = manager
        .activate(&manifest(Id128::generate(), TRIVIAL_HOT, vec![]))
        .expect_err("an exhausted budget must fail activation");
    assert!(
        matches!(err, ModuleError::Activation(ref m) if m.contains("budget")),
        "expected an activation budget failure, got {err:?}"
    );
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
    let vm = load_vm();
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
    let vm = load_vm();
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
        b.runtime.hot("kb_hot", "5").unwrap().unwrap(),
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
    let vm = load_vm();
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

const PUBLISHES_SVC_AND_UI: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"svc"}', 1, '[]')
  ctx.contribution_publish('{"kind":"ui","name":"panel","component":"panel_ui"}')
end
function kb_hot(x) return x end
"#;

/// Publishes a service and a UI mount, then fails the activation entry — the
/// C-F2 rollback case (effects staged before the failure).
const PUBLISHES_THEN_FAILS: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":["root"],"name":"svc"}', 1, '[]')
  ctx.contribution_publish('{"kind":"ui","name":"panel","component":"panel_ui"}')
  error("activation boom")
end
function kb_hot(x) return x end
"#;

/// C-F2/R-02: a failed activation may publish services/contributions before
/// failing; rollback must unpublish them, not leave a dead generation's
/// effects live.
#[test]
fn failed_activation_does_not_leak_services_or_contributions() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("activate-rollback", vm);
    let id = Id128::generate();
    let err = manager
        .activate(&manifest(id, PUBLISHES_THEN_FAILS, vec![]))
        .expect_err("activation must fail");
    assert!(matches!(err, ModuleError::Activation(_)), "{err:?}");

    assert!(
        manager.services().lock().unwrap().snapshot().is_empty(),
        "a failed activation must not leave services published"
    );
    assert_eq!(
        manager.ui_generation("panel_ui"),
        None,
        "a failed activation must not leave UI mounts live"
    );
    assert!(manager.snapshot().is_empty(), "nothing may stay registered");
    drop(manager);
    cleanup(dir, queue);
}

/// A1/R-02: a forced (vm-timeout) retirement must unpublish the generation's
/// services and contributions, not leave a dead generation's effects live.
#[test]
fn retire_unpublishes_generation_services_and_contributions() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("retire-unpublish", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, PUBLISHES_SVC_AND_UI, vec![]))
        .unwrap();
    let generation = g.generation;
    assert_eq!(manager.services().lock().unwrap().snapshot().len(), 1);
    assert_eq!(manager.published_contributions(generation).len(), 1);
    assert_eq!(manager.ui_generation("panel_ui"), Some(generation));

    manager.host().retire(generation, "test: forced retirement");

    assert!(
        manager.services().lock().unwrap().snapshot().is_empty(),
        "a retired generation's service must not stay published"
    );
    assert!(
        manager.published_contributions(generation).is_empty(),
        "a retired generation's contributions must be dropped"
    );
    assert_eq!(manager.ui_generation("panel_ui"), None);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// A1/R-02: direct disposal likewise must not leave the generation's effects
/// published.
#[test]
fn dispose_unpublishes_generation_services_and_contributions() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("dispose-unpublish", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, PUBLISHES_SVC_AND_UI, vec![]))
        .unwrap();
    let generation = g.generation;

    let rec = g.dispose();

    assert_eq!(rec.generation, generation);
    assert!(manager.services().lock().unwrap().snapshot().is_empty());
    assert!(manager.published_contributions(generation).is_empty());
    assert_eq!(manager.ui_generation("panel_ui"), None);
    drop(manager);
    cleanup(dir, queue);
}

/// T19/R-24/C-04: a generation whose actor is wedged (blocked in a host op)
/// cannot be force-killed — the drain detaches it, the shared abandoned-drain
/// counter records it, and the store stays resident on the abandoned thread.
#[test]
fn wedged_generation_actor_is_detached_and_recorded() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("wedged", vm);
    let id = Id128::generate();
    // `T4_HOST_CALL`'s `kb_hot` issues a `state_get` host op; holding the state
    // lock wedges the actor inside that call (the host import runs on a bounded
    // T6 worker, so the actor is blocked awaiting its reply).
    let g = manager.activate(&manifest(id, T4_HOST_CALL, vec![])).unwrap();
    let runtime = Arc::clone(&g.runtime);
    let state = manager.state();
    let guard = state.lock().unwrap();

    let caller = std::thread::spawn({
        let runtime = Arc::clone(&runtime);
        move || runtime.hot("kb_hot", "{}")
    });
    // Wait until the actor is executing the call before draining.
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.in_flight() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(runtime.in_flight() > 0, "actor never picked up the call");

    // The drain deadline elapses while the actor is blocked: detach + record.
    assert!(!runtime.shutdown(Duration::from_millis(100)));
    assert_eq!(manager.leaked_threads(), 1);

    // Unblock; the actor finishes the call, then exits on the queued Shutdown.
    drop(guard);
    assert!(caller.join().unwrap().unwrap().is_ok());
    drop(state);

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T19: the vm's `retire` is a non-blocking stop request; a later drain must
/// report a clean join (the actor left on its own), not a phantom wedge.
#[test]
fn shutdown_after_a_nonblocking_retire_is_a_clean_join() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("retire-then-drain", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, TRIVIAL_HOT, vec![])).unwrap();
    let runtime = Arc::clone(&g.runtime);

    // The vm's retire path: invalidate the token, unpublish, remove from the
    // table, and ask the actor to stop without waiting.
    manager.host().retire(g.generation, "test: non-blocking retire");
    assert!(!manager.generation_current(g.generation));

    // Whatever the actor's timing, this drain rejoins it cleanly.
    assert!(runtime.shutdown(Duration::from_secs(5)));
    assert_eq!(manager.leaked_threads(), 0);
    assert!(matches!(runtime.hot("kb_hot", "{}"), Err(ActorError::Gone)));

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T19: deactivating a generation the vm already retired must report the truth
/// — this drain did not join it — rather than a clean join.
#[test]
fn deactivate_after_a_vm_retire_reports_already_retired() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("deactivate-after-retire", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, TRIVIAL_HOT, vec![])).unwrap();
    let runtime = Arc::clone(&g.runtime);

    // The vm's forced-retirement path removes the actor from the table and
    // requests a stop without joining.
    manager.host().retire(g.generation, "test: forced retirement");

    let rec = manager.deactivate(id).unwrap();
    assert_eq!(rec.generation, g.generation);
    assert!(!rec.forced);
    assert!(rec.reason.contains("already retired"), "got: {}", rec.reason);

    // The actor still exits cleanly on the queued stop request.
    assert!(runtime.shutdown(Duration::from_secs(5)));
    assert_eq!(manager.leaked_threads(), 0);

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// R-07/C-F1: activation validates the manifest's declared state schema against
/// the existing head for its bound `state_key` — incompatible ⇒ rejected
/// atomically (typed error, old head untouched, nothing registered).
#[test]
fn activation_rejects_an_incompatible_state_schema() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("state-schema-reject", vm);
    // A first generation binds "planner" at schema 1 and writes a head.
    let mut first = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    first.state_key = Some("planner".into());
    first.state_schema = Some(1);
    let g = manager.activate(&first).unwrap();
    manager
        .state()
        .lock()
        .unwrap()
        .cas(StateUpdate {
            key: "planner".into(),
            schema: 1,
            bytes: br#"{"attempts":3}"#.to_vec(),
            generation: g.generation,
        })
        .unwrap();

    // A later generation declaring schema 2 must be rejected before it registers.
    let mut second = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    second.state_key = Some("planner".into());
    second.state_schema = Some(2);
    let err = manager
        .activate(&second)
        .expect_err("schema 2 vs head 1 must reject");
    assert!(
        matches!(err, ModuleError::State(StateError::SchemaMismatch { ref key, expected: 2, actual: 1 }) if key == "planner"),
        "{err:?}"
    );
    // The old head is untouched and only the first module is registered.
    let head = manager.state().lock().unwrap().get("planner").unwrap().unwrap().0;
    assert_eq!(head.schema, 1);
    assert_eq!(manager.snapshot().len(), 1);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// R-07/C-F1: a matching schema activates; `state_key` and `state_schema` must
/// be declared together.
#[test]
fn activation_accepts_a_matching_state_schema() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("state-schema-accept", vm);
    let mut first = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    first.state_key = Some("planner".into());
    first.state_schema = Some(1);
    let g = manager.activate(&first).unwrap();
    manager
        .state()
        .lock()
        .unwrap()
        .cas(StateUpdate {
            key: "planner".into(),
            schema: 1,
            bytes: br#"{"attempts":3}"#.to_vec(),
            generation: g.generation,
        })
        .unwrap();

    let mut compatible = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    compatible.state_key = Some("planner".into());
    compatible.state_schema = Some(1);
    manager
        .activate(&compatible)
        .expect("a matching schema must activate");

    // A declared schema without a bound key is a typed rejection, not a silently
    // dead field.
    let mut unbound = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    unbound.state_schema = Some(1);
    let err = manager.activate(&unbound).unwrap_err();
    assert!(matches!(err, ModuleError::InvalidInput(_)), "{err:?}");
    // A bound key without a declared schema is likewise rejected.
    let mut unbound2 = manifest(Id128::generate(), TRIVIAL_HOT, vec![]);
    unbound2.state_key = Some("planner".into());
    let err = manager.activate(&unbound2).unwrap_err();
    assert!(matches!(err, ModuleError::InvalidInput(_)), "{err:?}");
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// R-07/C-F1: once a module binds a state key, its writes to that key must use
/// the declared schema — a module cannot create a head its own manifest would
/// reject at the next activation.
#[test]
fn declared_state_schema_is_authoritative_on_writes() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("state-schema-write", vm);
    // T2_ACTIVATE writes `planner` at schema 1; declaring schema 2 must fail the
    // activation write (and roll back).
    let mut m = manifest(Id128::generate(), T2_ACTIVATE, vec![]);
    m.state_key = Some("planner".into());
    m.state_schema = Some(2);
    let err = manager
        .activate(&m)
        .expect_err("a write at the wrong schema must fail activation");
    assert!(
        matches!(err, ModuleError::Activation(ref msg) if msg.contains("does not match the declared module schema")),
        "{err:?}"
    );
    assert!(manager.snapshot().is_empty());
    drop(manager);
    cleanup(dir, queue);
}

/// R-07/C-07: `reset_head` starts a fresh head, returns the discarded one, and
/// resets the sequence; an absent head is a no-op.
#[test]
fn reset_head_starts_fresh_and_returns_the_old_head() {
    let (dir, mut state, queue) = state_store("reset-head");
    let h1 = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":1}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    let old = state.reset_head("k").unwrap().expect("an existing head");
    assert_eq!(old.digest, h1.digest);
    assert!(state.get("k").unwrap().is_none(), "reset starts a fresh head");
    // The next CAS starts at sequence 1 again.
    let h2 = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"b":2}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    assert_eq!(h2.seq, 1);
    // Resetting an absent head is a no-op.
    assert!(state.reset_head("absent").unwrap().is_none());
    drop(state);
    cleanup(dir, queue);
}

/// R-07: `restore_head` puts a reset head back verbatim (the reset-rollback
/// primitive), preserving digest and sequence.
#[test]
fn restore_head_puts_a_reset_head_back() {
    let (dir, mut state, queue) = state_store("restore-head");
    let h = state
        .cas(StateUpdate {
            key: "k".into(),
            schema: 1,
            bytes: br#"{"a":1}"#.to_vec(),
            generation: 1,
        })
        .unwrap();
    state.reset_head("k").unwrap();
    assert!(state.get("k").unwrap().is_none());
    state.restore_head("k", &h).unwrap();
    let (back, bytes) = state.get("k").unwrap().unwrap();
    assert_eq!(back.digest, h.digest);
    assert_eq!(back.seq, h.seq);
    assert_eq!(bytes, br#"{"a":1}"#.to_vec());
    drop(state);
    cleanup(dir, queue);
}

// --- T9 hooks: multiplexed named entry points + call_hook + respawn --------

/// A module declaring both hooks. Its `kb_hot` stays a normal hot entry (the
/// multiplexer only intercepts the hook envelope); `kb_name` is the stable
/// hook contribution name.
const HOOK_MODULE: &str = r#"
kb_name = "hook_mod"
function kb_on_activate(ctx) end
function kb_hot(x)
  if type(x) == "table" and x.kind == "plain" then return "plain" end
  return "orig"
end
function kb_on_turn_start(context)
  return { decision = context.decision or "continue" }
end
function kb_on_tool_intent(context)
  return { decision = "deny", tool = context.tool }
end
"#;

/// A hook that blocks in a host op (used to wedge the actor) — mirrors
/// `T4_HOST_CALL` but on the hook entry.
const WEDGE_HOOK: &str = r#"
kb_name = "wedge_mod"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  return kb_host_call(1, '{"key":"planner"}')
end
"#;

/// A hook that never returns (fuel exhaustion → trap).
const TRAP_HOOK: &str = r#"
kb_name = "trap_mod"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  local n = 0
  while true do n = n + 1 end
end
"#;

/// A hook returning a non-serializable value (a function) — the guest's JSON
/// serializer rejects it, surfacing a guest return error.
const BAD_RESULT_HOOK: &str = r#"
kb_name = "bad_mod"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  return function() end
end
"#;

/// A module whose activation performs a host op, so holding the shared state
/// lock wedges its (re)activation (NEW-3 regression).
const SLOW_ACTIVATE: &str = r#"
function kb_on_activate(ctx)
  kb_host_call(1, '{"key":"slow_activate"}')
end
function kb_hot(x) return "hot" end
"#;

fn hook_vm() -> Vm {
    match Vm::load(no_epoch()) {
        Ok(vm) => vm,
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

/// T9: the activation shim discovers declared hooks, stages a hook
/// contribution, and the `kb_hot` multiplexer dispatches the kernel envelope
/// to the named entry — a deny/continue decision comes back to the caller.
#[test]
fn hook_module_dispatches_named_entry_and_stages_contribution() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-dispatch", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, HOOK_MODULE, vec![]))
        .unwrap();

    // The shim staged exactly the two declared hooks.
    let published = manager.published_contributions(g.generation);
    let hooks: Vec<_> = published
        .iter()
        .filter_map(|c| match &c.kind {
            ContributionKind::Hook(h) => Some((h.hook, h.name.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(hooks.len(), 2, "both hooks declared: {hooks:?}");
    assert!(hooks
        .iter()
        .any(|(k, n)| *k == HookKind::OnTurnStart && n == "hook_mod"));
    assert!(hooks
        .iter()
        .any(|(k, n)| *k == HookKind::OnToolIntent && n == "hook_mod"));
    assert_eq!(
        manager.hook_generation(&root(), HookKind::OnTurnStart, "hook_mod"),
        Some(g.generation)
    );

    // The envelope dispatches to kb_on_turn_start; the decision comes back.
    let out = manager
        .call_hook(g.generation, HookKind::OnTurnStart, r#"{"decision":"deny"}"#, HOOK_WAIT)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "deny");

    // And to kb_on_tool_intent.
    let out = manager
        .call_hook(g.generation, HookKind::OnToolIntent, r#"{"tool":"rm"}"#, HOOK_WAIT)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "deny");
    assert_eq!(v["tool"], "rm");

    // Anything else falls through to the original kb_hot (byte-identical
    // behavior for the non-hook path).
    let out = manager.call_generation(g.generation, r#"{"kind":"plain"}"#).unwrap();
    assert_eq!(out, "\"plain\"");

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9: a plain module (no declared hooks) keeps its original `kb_hot` — the
/// multiplexer installs nothing.
#[test]
fn module_without_hooks_keeps_original_kb_hot() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-plain", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, TRIVIAL_HOT, vec![])).unwrap();

    // No hook contributions are staged.
    assert!(
        manager
            .published_contributions(g.generation)
            .iter()
            .all(|c| !matches!(c.kind, ContributionKind::Hook(_)))
    );
    // The original kb_hot still answers (`x * 2`).
    assert_eq!(manager.call_generation(g.generation, "21").unwrap(), "42");

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9: a wedged actor surfaces `HookError::Timeout` within the bound.
#[test]
fn wedged_hook_times_out_within_the_bound() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-wedge", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, WEDGE_HOOK, vec![])).unwrap();
    let state = manager.state();
    let guard = state.lock().unwrap();

    let started = Instant::now();
    let err = manager
        .call_hook(
            g.generation,
            HookKind::OnTurnStart,
            "{}",
            Duration::from_millis(100),
        )
        .unwrap_err();
    assert_eq!(err, HookError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the hook call must return within the bound"
    );

    // Unblock so the actor can finish and be drained cleanly.
    drop(guard);
    drop(state);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9: a trapping hook (fuel exhaustion) surfaces `HookError::Trap`.
#[test]
fn trapping_hook_returns_trap() {
    // Enough fuel for the shim + activation, little enough that the hook's
    // infinite loop exhausts it.
    let vm = Vm::load(VmConfig {
        fuel_per_call: 20_000_000,
        epoch_deadline: u64::MAX,
        ..Default::default()
    })
    .unwrap();
    let (dir, mut manager, queue) = manager_setup("hook-trap", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, TRAP_HOOK, vec![])).unwrap();
    let err = manager
        .call_hook(g.generation, HookKind::OnTurnStart, "{}", HOOK_WAIT)
        .unwrap_err();
    assert_eq!(err, HookError::Trap);
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9: a non-serializable hook result and an invalid context both surface as
/// `HookError::Invalid`; a well-formed but semantically malformed decision is
/// returned raw for the session to classify.
#[test]
fn invalid_hook_inputs_and_results_are_structured() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-invalid", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, BAD_RESULT_HOOK, vec![]))
        .unwrap();

    // Invalid context JSON is a KERNEL-side bug: a distinct error that the
    // session must not treat as a guest fault (H).
    assert_eq!(
        manager
            .call_hook(g.generation, HookKind::OnTurnStart, "not json", HOOK_WAIT)
            .unwrap_err(),
        HookError::InvalidContext
    );
    // A non-serializable guest result is a guest return error → Invalid.
    assert_eq!(
        manager
            .call_hook(g.generation, HookKind::OnTurnStart, "{}", HOOK_WAIT)
            .unwrap_err(),
        HookError::Invalid
    );

    drop(g);
    drop(manager);
    cleanup(dir, queue);

    // A raw (session-classified) malformed decision is returned as-is.
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-raw", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, HOOK_MODULE, vec![]))
        .unwrap();
    let raw = manager
        .call_hook(g.generation, HookKind::OnTurnStart, "{}", HOOK_WAIT)
        .unwrap();
    // Valid JSON, but not an object the session can read a decision from.
    assert_eq!(serde_json::from_str::<serde_json::Value>(&raw).unwrap()["decision"], "continue");
    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9: respawn yields a new generation id, the old actor is unavailable, the
/// composition is unchanged, and the module's `kb_hot` works afterward.
#[test]
fn respawn_rotates_generation_and_preserves_composition() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("respawn", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, HOOK_MODULE, vec![]))
        .unwrap();
    let old_generation = g.generation;
    let before = manager.published_contributions(old_generation);
    let before_snapshot = manager.snapshot();

    let new_generation = manager.respawn(id).unwrap();
    assert_ne!(new_generation, old_generation, "generation ids are never reused");
    assert!(!manager.generation_current(old_generation));
    assert!(manager.call_generation(old_generation, "{}").is_err());
    assert_eq!(manager.snapshot()[0].1, new_generation);

    // Same package → identical staged contributions and digest.
    assert_eq!(manager.published_contributions(new_generation), before);
    assert_eq!(manager.snapshot()[0].2, before_snapshot[0].2);
    assert_eq!(
        manager.hook_generation(&root(), HookKind::OnTurnStart, "hook_mod"),
        Some(new_generation)
    );

    // The new generation's multiplexed hook and plain hot both work.
    let out = manager
        .call_hook(
            new_generation,
            HookKind::OnTurnStart,
            r#"{"decision":"continue"}"#,
            HOOK_WAIT,
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["decision"], "continue");
    assert_eq!(
        manager.call_generation(new_generation, r#"{"kind":"plain"}"#).unwrap(),
        "\"plain\""
    );

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// T9/D: a peer module's spoofed `__kb_hook` envelope (no nonce, or a wrong
/// one) falls through to the real `kb_hot` instead of hijacking the hook; the
/// kernel's own hook call presents the per-generation secret and dispatches.
#[test]
fn hook_call_is_nonce_authenticated() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-nonce", vm);
    let id = Id128::generate();
    let g = manager
        .activate(&manifest(id, HOOK_MODULE, vec![]))
        .unwrap();

    // The kernel's own hook call dispatches to the hook entry.
    let out = manager
        .call_hook(
            g.generation,
            HookKind::OnToolIntent,
            r#"{"tool":"rm"}"#,
            HOOK_WAIT,
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["decision"], "deny");

    // A `service_call`-style payload cannot know the nonce, so it reaches the
    // module's original `kb_hot` (which returns "orig").
    for spoof in [
        r#"{"__kb_hook":"on_tool_intent","context":{"tool":"rm"}}"#,
        r#"{"__kb_hook":"on_tool_intent","__kb_nonce":"guess","context":{"tool":"rm"}}"#,
    ] {
        assert_eq!(
            manager.call_generation(g.generation, spoof).unwrap(),
            "\"orig\"",
            "spoof must fall through: {spoof}"
        );
    }

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// NEW-1: an untrusted origin cannot squat a hook key and suppress a trusted
/// module's hook. The untrusted publish is ignored at the host boundary, so it
/// leaves no occupied entry and the trusted module binds regardless of
/// activation order.
#[test]
fn untrusted_hook_cannot_squat_a_trusted_hook_key() {
    let vm = hook_vm();
    let (dir, mut manager, queue) = manager_setup("hook-squat", vm);

    // Untrusted first: it must not occupy (root, OnTurnStart, "hook_mod").
    let mut untrusted = manifest(Id128::generate(), HOOK_MODULE, vec![]);
    untrusted.origin = ModuleOrigin::WorkspaceConfig;
    let squatter = manager.activate(&untrusted).unwrap();
    assert_eq!(
        manager.hook_generation(&root(), HookKind::OnTurnStart, "hook_mod"),
        None,
        "an untrusted hook publish must not occupy a key"
    );
    assert!(
        manager
            .published_contributions(squatter.generation)
            .iter()
            .all(|c| !matches!(c.kind, ContributionKind::Hook(_))),
        "an untrusted hook contribution must not be staged"
    );

    // A trusted module with the same key still binds to its own generation.
    let trusted = manager
        .activate(&manifest(Id128::generate(), HOOK_MODULE, vec![]))
        .unwrap();
    assert_eq!(
        manager.hook_generation(&root(), HookKind::OnTurnStart, "hook_mod"),
        Some(trusted.generation),
        "the trusted hook must bind despite the earlier untrusted publish"
    );
    let out = manager
        .call_hook(
            trusted.generation,
            HookKind::OnTurnStart,
            r#"{"decision":"deny"}"#,
            HOOK_WAIT,
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["decision"],
        "deny"
    );

    drop(squatter);
    drop(trusted);
    drop(manager);
    cleanup(dir, queue);
}

/// NEW-3: a respawn whose `kb_on_activate` wedges is bounded by the respawn
/// activation budget, not the 10s reply timeout — a hook-fault respawn must not
/// stall a decision for seconds.
#[test]
fn respawn_activation_is_bounded() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("respawn-bounded", vm);
    let id = Id128::generate();
    // Activation calls a host op, so holding the state lock wedges the
    // respawn-time activation.
    let g = manager.activate(&manifest(id, SLOW_ACTIVATE, vec![])).unwrap();

    let state = manager.state();
    let guard = state.lock().unwrap();
    let started = Instant::now();
    let err = manager
        .respawn_bounded(id, Duration::from_millis(200))
        .unwrap_err();
    let elapsed = started.elapsed();
    drop(guard);
    drop(state);

    assert!(
        matches!(err, ModuleError::Activation(_)),
        "a wedged respawn activation must fail, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "respawn activation must be bounded, not the 10s reply timeout: {elapsed:?}"
    );

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}

/// NEW-6: a retire while the actor is busy must not lose the stop request. The
/// actor's command channel is full, so only the sticky stop flag can reach it.
#[test]
fn retire_while_busy_is_not_dropped() {
    let vm = load_vm();
    let (dir, mut manager, queue) = manager_setup("retire-busy", vm);
    let id = Id128::generate();
    let g = manager.activate(&manifest(id, T4_HOST_CALL, vec![])).unwrap();
    let runtime = Arc::clone(&g.runtime);

    // Hold the state lock so the actor's host call blocks; a second request
    // then fills the capacity-1 command channel.
    let state = manager.state();
    let guard = state.lock().unwrap();
    let busy = {
        let r = Arc::clone(&runtime);
        std::thread::spawn(move || {
            let _ = r.hot("kb_hot", "{}");
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let queued = {
        let r = Arc::clone(&runtime);
        std::thread::spawn(move || {
            let _ = r.hot("kb_hot", "{}");
        })
    };
    std::thread::sleep(Duration::from_millis(50));

    // Retire while busy: `try_send` cannot enqueue the `Shutdown`.
    manager.host().retire(g.generation, "test: retire while busy");
    drop(guard);
    drop(state);
    busy.join().unwrap();
    queued.join().unwrap();
    // Give the actor a poll interval to observe the sticky stop flag.
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        matches!(runtime.hot("kb_hot", "{}"), Err(ActorError::Gone)),
        "the actor must have stopped even though its channel was full"
    );

    drop(g);
    drop(manager);
    cleanup(dir, queue);
}
