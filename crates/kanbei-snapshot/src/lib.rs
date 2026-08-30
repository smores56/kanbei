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

/// Manifest schema version (M4: 4 — project memory root; M3: 3 —
/// tool-registry/provider/scheduler pins; 2 — module pins + composition
/// digest).
pub const MANIFEST_SCHEMA: u32 = 4;

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
    /// Project-scoped memory claim root (M4); lifetime root stays in
    /// memory_root. None when the session has no project binding.
    #[serde(default)]
    pub project_memory_root: Option<Digest>,
    /// Tool-registry snapshot digest; None until M3 (schema 3 pin).
    #[serde(default)]
    pub tool_registry: Option<Digest>,
    /// Projection version/watermark; None in M1.
    #[serde(default)]
    pub projection: Option<u64>,
    /// Provider config digest (provider/model/key-source fingerprint, never
    /// the key); None until M3.
    #[serde(default)]
    pub provider_config: Option<Digest>,
    /// Scheduler policy name (R-09/E-09 canonical surface); None until M3.
    #[serde(default)]
    pub scheduler_policy: Option<String>,
    /// Cognition scheduler/provider version; None until M3.
    #[serde(default)]
    pub provider: Option<u64>,
    /// Retention-policy version; None until M2.
    #[serde(default)]
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
            project_memory_root: None,
            tool_registry: None,
            projection: None,
            provider_config: None,
            scheduler_policy: None,
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

/// The complete referenced digest set of a manifest — every digest field the
/// pinned manifest's closure must contain: `engine_digest`, `toolchain_digest`,
/// `state_head`, `composition`, `memory_root`, `project_memory_root`,
/// `tool_registry`, `provider_config`, and every `modules[].package` (M6
/// wave 2 full closure walk; the wave-1 manual set covered only packages +
/// composition + memory roots).
pub fn manifest_closure(m: &ExecutionManifest) -> HashSet<Digest> {
    let mut refs: HashSet<Digest> = m.modules.iter().map(|pin| pin.package).collect();
    for d in [
        m.engine_digest,
        m.toolchain_digest,
        m.state_head,
        m.composition,
        m.memory_root,
        m.project_memory_root,
        m.tool_registry,
        m.provider_config,
    ]
    .into_iter()
    .flatten()
    {
        refs.insert(d);
    }
    refs
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
