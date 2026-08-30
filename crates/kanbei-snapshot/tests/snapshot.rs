//! Integration tests for kanbei-snapshot: manifest pinning, dedup,
//! closure verification, and the kernel bootstrap shape.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::ENVELOPE_SCHEMA;
use kanbei_core::queue::DurabilityQueue;
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_core::id::Id128;
use kanbei_snapshot::{
    ExecutionManifest, MANIFEST_SCHEMA, ModulePin, manifest_closure, pin, verify_closure,
};

/// Temp store with a named tag; caller must shutdown the returned queue.
fn store(tag: &str) -> (PathBuf, ObjectStore, Arc<DurabilityQueue>) {
    let dir = std::env::temp_dir().join(format!("kb-snapshot-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let queue = Arc::new(DurabilityQueue::start(&format!("test-snapshot-{tag}")));
    let store = ObjectStore::open(&dir, Arc::clone(&queue)).unwrap();
    (dir, store, queue)
}

fn shutdown(dir: &PathBuf, store: ObjectStore, queue: Arc<DurabilityQueue>) {
    drop(store);
    Arc::try_unwrap(queue)
        .unwrap_or_else(|_| panic!("queue Arc still shared"))
        .shutdown()
        .unwrap();
    let _ = fs::remove_dir_all(dir);
}

/// A manifest distinct from bootstrap: pinned state head.
fn with_state_head(head: &[u8]) -> ExecutionManifest {
    let mut m = ExecutionManifest::bootstrap();
    m.state_head = Some(Digest::new(head));
    m
}

#[test]
fn roundtrip_bootstrap_pin_get_deserialize() {
    let (dir, mut store, queue) = store("roundtrip");
    let m = ExecutionManifest::bootstrap();
    let (digest, deduped) = pin(&mut store, &m).unwrap();
    store.flush().unwrap();
    assert!(!deduped);
    let bytes = store.get(&digest).unwrap();
    let got: ExecutionManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(got, m);
    shutdown(&dir, store, queue);
}

#[test]
fn dedup_same_manifest_pins_once() {
    let (dir, mut store, queue) = store("dedup");
    let m = ExecutionManifest::bootstrap();
    let (d1, deduped1) = pin(&mut store, &m).unwrap();
    let (d2, deduped2) = pin(&mut store, &m).unwrap();
    store.flush().unwrap();
    assert_eq!(d1, d2);
    assert!(!deduped1);
    assert!(deduped2);
    assert_eq!(store.installs, 1);
    shutdown(&dir, store, queue);
}

#[test]
fn distinct_state_head_distinct_digest() {
    let (dir, mut store, queue) = store("distinct");
    let m1 = with_state_head(b"head-a");
    let m2 = with_state_head(b"head-b");
    let (d1, _) = pin(&mut store, &m1).unwrap();
    let (d2, _) = pin(&mut store, &m2).unwrap();
    store.flush().unwrap();
    assert_ne!(d1, d2);
    shutdown(&dir, store, queue);
}

#[test]
fn verify_closure_all_present() {
    let (dir, mut store, queue) = store("closure-ok");
    let manifests = [
        ExecutionManifest::bootstrap(),
        with_state_head(b"head-1"),
        with_state_head(b"head-2"),
    ];
    let mut refs = HashSet::new();
    for m in &manifests {
        let (d, _) = pin(&mut store, m).unwrap();
        refs.insert(d);
    }
    store.flush().unwrap();
    assert_eq!(verify_closure(&store, &refs).unwrap(), 3);
    shutdown(&dir, store, queue);
}

#[test]
fn verify_closure_missing_ref() {
    let (dir, mut store, queue) = store("closure-missing");
    let (d, _) = pin(&mut store, &ExecutionManifest::bootstrap()).unwrap();
    store.flush().unwrap();
    let ghost = Digest::new(b"never installed");
    assert_ne!(ghost, d);
    let refs = HashSet::from([d, ghost]);
    match verify_closure(&store, &refs) {
        Err(ObjectError::Missing { digest }) => assert_eq!(digest, ghost),
        other => panic!("expected Missing naming {ghost}, got {other:?}"),
    }
    shutdown(&dir, store, queue);
}

#[test]
fn verify_closure_corrupt_object() {
    let (dir, mut store, queue) = store("closure-corrupt");
    let (d, _) = pin(&mut store, &ExecutionManifest::bootstrap()).unwrap();
    store.flush().unwrap();
    // Overwrite the object file with garbage; get() must detect the hash mismatch.
    fs::write(store.path_for(&d), b"garbage").unwrap();
    let refs = HashSet::from([d]);
    match verify_closure(&store, &refs) {
        Err(ObjectError::Corruption {
            digest,
            expected,
            actual,
        }) => {
            assert_eq!(digest, d);
            assert_eq!(expected, d);
            assert_ne!(actual, expected);
        }
        other => panic!("expected Corruption, got {other:?}"),
    }
    shutdown(&dir, store, queue);
}

#[test]
fn manifest_closure_covers_all_digest_fields() {
    let mut m = ExecutionManifest::bootstrap();
    m.engine_digest = Some(Digest::new(b"engine"));
    m.toolchain_digest = Some(Digest::new(b"toolchain"));
    m.state_head = Some(Digest::new(b"state"));
    m.composition = Some(Digest::new(b"composition"));
    m.memory_root = Some(Digest::new(b"memory"));
    m.project_memory_root = Some(Digest::new(b"project-memory"));
    m.tool_registry = Some(Digest::new(b"tools"));
    m.provider_config = Some(Digest::new(b"provider"));
    m.modules = vec![
        ModulePin {
            module_id: Id128::generate(),
            generation: 1,
            package: Digest::new(b"package-1"),
            scope: "/".into(),
        },
        ModulePin {
            module_id: Id128::generate(),
            generation: 2,
            package: Digest::new(b"package-2"),
            scope: "/".into(),
        },
    ];
    let refs = manifest_closure(&m);
    for d in [
        m.engine_digest.unwrap(),
        m.toolchain_digest.unwrap(),
        m.state_head.unwrap(),
        m.composition.unwrap(),
        m.memory_root.unwrap(),
        m.project_memory_root.unwrap(),
        m.tool_registry.unwrap(),
        m.provider_config.unwrap(),
        m.modules[0].package,
        m.modules[1].package,
    ] {
        assert!(refs.contains(&d), "closure missing {d}");
    }
    // No digest field escapes the closure: with every field Some the set is
    // exactly the ten digests above.
    assert_eq!(refs.len(), 10);
    // A bare manifest has an empty closure.
    assert!(manifest_closure(&ExecutionManifest::bootstrap()).is_empty());
}

#[test]
fn bootstrap_shape() {
    let m = ExecutionManifest::bootstrap();
    assert_eq!(m.schema, 4);
    assert_eq!(m.schema, MANIFEST_SCHEMA);
    assert_eq!(m.kernel_schema, 1);
    assert_eq!(m.envelope_schema, ENVELOPE_SCHEMA);
    assert_eq!(m.module_abi, Some(1));
    assert_eq!(m.state_head, None);
    assert_eq!(m.modules, Vec::new());
    assert_eq!(m.composition, None);
    assert_eq!(m.engine_digest, None);
    assert_eq!(m.toolchain_digest, None);
    assert_eq!(m.memory_root, None);
    assert_eq!(m.project_memory_root, None);
    assert_eq!(m.tool_registry, None);
    assert_eq!(m.projection, None);
    assert_eq!(m.provider_config, None);
    assert_eq!(m.scheduler_policy, None);
    assert_eq!(m.provider, None);
    assert_eq!(m.policy, None);
    assert_eq!(m.schema_versions, vec![1]);
}

#[test]
fn project_memory_root_roundtrips_and_pre_m4_deserializes() {
    let project_root = Digest::new(b"project-root");
    let mut m = ExecutionManifest::bootstrap();
    m.project_memory_root = Some(project_root);
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"project_memory_root\""));
    let back: ExecutionManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.project_memory_root, Some(project_root));

    // Schema-3 manifests (no field) still deserialize via serde default.
    let pre_m4 = json.replace(&format!(",\"project_memory_root\":\"{project_root}\""), "");
    assert!(!pre_m4.contains("project_memory_root"));
    let back: ExecutionManifest = serde_json::from_str(&pre_m4).unwrap();
    assert_eq!(back.schema, MANIFEST_SCHEMA);
    assert_eq!(back.project_memory_root, None);
    assert_eq!(back.memory_root, m.memory_root);
}

#[test]
fn to_bytes_stable_and_parses() {
    let m = with_state_head(b"stable");
    let a = m.to_bytes();
    let b = m.to_bytes();
    assert_eq!(a, b);
    let got: ExecutionManifest = serde_json::from_slice(&a).unwrap();
    assert_eq!(got, m);
}

#[test]
fn json_contains_version_fields() {
    let m = with_state_head(b"fields");
    let json = String::from_utf8(m.to_bytes()).unwrap();
    for key in [
        "\"schema\"",
        "\"kernel_schema\"",
        "\"envelope_schema\"",
        "\"module_abi\"",
        "\"engine_digest\"",
        "\"toolchain_digest\"",
        "\"state_head\"",
        "\"modules\"",
        "\"composition\"",
        "\"project_memory_root\"",
        "\"schema_versions\"",
    ] {
        assert!(json.contains(key), "manifest JSON missing {key}: {json}");
    }
}
