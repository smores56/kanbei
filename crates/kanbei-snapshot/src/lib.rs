//! kanbei-snapshot — the execution-snapshot manifest (ratification-packet §6,
//! architecture.md R-08): an acyclic digest/ID structure pinning the kernel
//! bootstrap versions and environment pins (module/state/memory/tool/
//! projection/provider/policy) at state-changing event commits. Manifests are
//! content-addressed: identical manifests dedup to the same object, and closure
//! verification hash-verifies every referenced object.

use std::collections::HashSet;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::ENVELOPE_SCHEMA;
use kanbei_core::id::Id128;
use kanbei_objects::{ObjectError, ObjectStore};
use serde::{Deserialize, Serialize};

/// Manifest schema version (M2: 2 — module pins + composition digest).
pub const MANIFEST_SCHEMA: u32 = 2;

/// One active module generation pin: stable module id, generation, package
/// digest, and the scope it was activated in (M2: "/" — root scope only).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModulePin {
    pub module_id: Id128,
    pub generation: u64,
    pub package: Digest,
    pub scope: String,
}

/// Execution-snapshot manifest: environment pins + version fields.
/// Field order is the canonical JSON layout (derive order).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExecutionManifest {
    pub schema: u32,
    /// Kernel bootstrap schema version (R-08/E-12).
    pub kernel_schema: u32,
    /// Event-envelope schema version.
    pub envelope_schema: u32,
    /// Module ABI version (M2: 1).
    pub module_abi: Option<u32>,
    /// Wasm engine digest; Some when the session loaded the guest wasm (M2).
    pub engine_digest: Option<Digest>,
    /// Toolchain manifest digest; M2 sessions do not track a toolchain yet.
    pub toolchain_digest: Option<Digest>,
    /// Module state head; None until M2, populated by session state changes.
    pub state_head: Option<Digest>,
    /// Active module-generation pins (M2), sorted by module_id.
    pub modules: Vec<ModulePin>,
    /// Epoch composition digest (R-01); None until M2.
    pub composition: Option<Digest>,
    /// Memory claim root; M4 — always None in M1.
    pub memory_root: Option<Digest>,
    /// Tool-registry snapshot digest; None until M2.
    pub tool_registry: Option<Digest>,
    /// Projection version/watermark; None in M1.
    pub projection: Option<u64>,
    /// Cognition scheduler/provider; None until M3.
    pub provider: Option<u64>,
    /// Retention-policy version; None until M2.
    pub policy: Option<u64>,
    /// Payload schemas known at pin time.
    pub schema_versions: Vec<u32>,
}

impl ExecutionManifest {
    /// Kernel bootstrap manifest — used for the genesis snapshot (R-08: "A
    /// genesis event uses an explicit kernel bootstrap snapshot").
    pub fn bootstrap() -> Self {
        Self {
            schema: MANIFEST_SCHEMA,
            kernel_schema: 1,
            envelope_schema: ENVELOPE_SCHEMA,
            module_abi: Some(1),
            engine_digest: None,
            toolchain_digest: None,
            state_head: None,
            modules: Vec::new(),
            composition: None,
            memory_root: None,
            tool_registry: None,
            projection: None,
            provider: None,
            policy: None,
            schema_versions: vec![1],
        }
    }

    /// Canonical JSON bytes (derive field order).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serialization cannot fail")
    }
}

/// Pin a manifest as an object. Content addressing deduplicates: an unchanged
/// manifest maps to the same digest and is not rewritten (S4 finding).
/// Returns (digest, deduped) where deduped = the object already existed.
pub fn pin(store: &mut ObjectStore, m: &ExecutionManifest) -> Result<(Digest, bool), ObjectError> {
    let bytes = m.to_bytes();
    let digest = Digest::new(&bytes);
    let deduped = store.exists(&digest);
    store.install(&bytes)?;
    Ok((digest, deduped))
}

/// Verify closure: every referenced object exists with a valid hash
/// (missing → `ObjectError::Missing`, damaged → `ObjectError::Corruption`;
/// never silent). Returns the count of verified objects.
pub fn verify_closure(store: &ObjectStore, refs: &HashSet<Digest>) -> Result<u64, ObjectError> {
    let mut verified = 0u64;
    for r in refs {
        store.get(r)?;
        verified += 1;
    }
    Ok(verified)
}
