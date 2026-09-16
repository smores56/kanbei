//! M2 packages: immutable module manifests installed as content-addressed
//! objects (architecture.md "Unified module lifecycle": stable ModuleId +
//! immutable content/package hash; activation canonicality R-01/C-01). The
//! Luau source is inline — M2 packages are small.

use std::io;

use kanbei_capabilities::{Capability, TrustClass};
use kanbei_core::id::Id128;
use kanbei_core::Digest;
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_services::{ScopePath, ServiceDependency};
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Package manifest schema version. 2 adds `state_key` (R-07/C-F1).
pub const PACKAGE_SCHEMA: u32 = 2;

/// Where a module came from (metadata only; trust enforcement is the
/// capability broker's job). Wire form is the snake_case variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleOrigin {
    Builtin,
    UserConfig,
    WorkspaceConfig,
    Agent,
    UserInstalled,
}

impl ModuleOrigin {
    fn name(self) -> &'static str {
        match self {
            ModuleOrigin::Builtin => "builtin",
            ModuleOrigin::UserConfig => "user_config",
            ModuleOrigin::WorkspaceConfig => "workspace_config",
            ModuleOrigin::Agent => "agent",
            ModuleOrigin::UserInstalled => "user_installed",
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "builtin" => Ok(ModuleOrigin::Builtin),
            "user_config" => Ok(ModuleOrigin::UserConfig),
            "workspace_config" => Ok(ModuleOrigin::WorkspaceConfig),
            "agent" => Ok(ModuleOrigin::Agent),
            "user_installed" => Ok(ModuleOrigin::UserInstalled),
            other => Err(format!("unknown module origin {other:?}")),
        }
    }

    /// Whether this origin is TRUSTED to publish sensitive contributions
    /// (settings fields, lifecycle hooks). `Builtin` and `UserConfig` are
    /// user-authorized; `WorkspaceConfig`/`Agent`/`UserInstalled` are repo-,
    /// agent-, or install-supplied, so the kernel gates their contributions
    /// (F2/B). One predicate shared by the settings gate and the hook gate.
    pub fn is_trusted(self) -> bool {
        matches!(self, ModuleOrigin::Builtin | ModuleOrigin::UserConfig)
    }

    /// Precedence rank for precedence-driven implicit replacement
    /// (decision 28): a contribution whose origin ranks HIGHER replaces the
    /// contribution that occupies the same identity key at a LOWER rank.
    ///
    /// The declared config layers are `Builtin < UserConfig < WorkspaceConfig`
    /// — project (workspace) config is more specific than user config, which is
    /// more specific than the built-in defaults. `Agent` and `UserInstalled`
    /// rank above all declared config: they are explicit, deliberate additions
    /// made at runtime (by the agent or by an install), so they should take a
    /// key over rather than be silently displaced by a discovered config file.
    pub fn precedence_rank(self) -> u8 {
        match self {
            ModuleOrigin::Builtin => 0,
            ModuleOrigin::UserConfig => 1,
            ModuleOrigin::WorkspaceConfig => 2,
            ModuleOrigin::Agent => 3,
            ModuleOrigin::UserInstalled => 4,
        }
    }
}

/// Wire form of `kanbei_capabilities::TrustClass` (that crate has no serde
/// impls; the name is stable here).
fn trust_class_name(t: TrustClass) -> &'static str {
    match t {
        TrustClass::User => "user",
        TrustClass::Workspace => "workspace",
        TrustClass::Agent => "agent",
        TrustClass::Builtin => "builtin",
    }
}

fn parse_trust_class(s: &str) -> Result<TrustClass, String> {
    match s {
        "user" => Ok(TrustClass::User),
        "workspace" => Ok(TrustClass::Workspace),
        "agent" => Ok(TrustClass::Agent),
        "builtin" => Ok(TrustClass::Builtin),
        other => Err(format!("unknown trust class {other:?}")),
    }
}

/// An immutable module package. Canonical JSON bytes (field order as declared)
/// are the package object; the object digest is the package hash.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageManifest {
    pub schema: u32,
    pub module_id: Id128,
    pub origin: ModuleOrigin,
    pub trust_class: TrustClass,
    pub scope: ScopePath,
    /// Services this module depends on, declared at install time (R-25/C-05);
    /// M2 uses them as the caller-side version contract for `service_call`.
    pub deps: Vec<ServiceDependency>,
    /// Capabilities the module requests; M2 records them, the broker grants
    /// decide.
    pub capabilities: Vec<Capability>,
    /// Inline Luau source. Contract: defines `kb_hot` (guest requirement) and
    /// `kb_on_activate(ctx)` (see `lifecycle::ACTIVATION_SHIM`); top-level
    /// code must be pure — it runs once in the cached VM and once in the
    /// activation VM.
    pub source: String,
    /// Declared module-state schema; M2 enforces schema continuity on the
    /// state head at CAS time (fail-closed, R-07/C-07).
    pub state_schema: Option<u32>,
    /// The module's designated state head key (R-07/C-F1). Activation validates
    /// the existing head's schema against `state_schema` (fail-closed, atomic,
    /// old head untouched); `module reset-state` reinitializes this head and
    /// records a canonical fact. `state_key` and `state_schema` are set
    /// together (or neither).
    pub state_key: Option<String>,
}

/// Wire form of a `Capability` (that crate has no serde impls): the
/// canonical `{"resource", "verbs"}` shape.
#[derive(Serialize)]
struct CapabilityWire<'a> {
    resource: &'a str,
    verbs: &'a [String],
}

impl Serialize for PackageManifest {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let capabilities: Vec<CapabilityWire> = self
            .capabilities
            .iter()
            .map(|c| CapabilityWire {
                resource: &c.resource,
                verbs: &c.verbs,
            })
            .collect();
        let mut st = ser.serialize_struct("PackageManifest", 10)?;
        st.serialize_field("schema", &self.schema)?;
        st.serialize_field("module_id", &self.module_id)?;
        st.serialize_field("origin", &self.origin.name())?;
        st.serialize_field("trust_class", &trust_class_name(self.trust_class))?;
        st.serialize_field("scope", &self.scope)?;
        st.serialize_field("deps", &self.deps)?;
        st.serialize_field("capabilities", &capabilities)?;
        st.serialize_field("source", &self.source)?;
        st.serialize_field("state_schema", &self.state_schema)?;
        st.serialize_field("state_key", &self.state_key)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for PackageManifest {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            schema: u32,
            module_id: Id128,
            origin: String,
            trust_class: String,
            scope: ScopePath,
            deps: Vec<ServiceDependency>,
            #[serde(deserialize_with = "deserialize_capability_vec")]
            capabilities: Vec<Capability>,
            source: String,
            state_schema: Option<u32>,
            #[serde(default)]
            state_key: Option<String>,
        }
        let wire = Wire::deserialize(de)?;
        Ok(PackageManifest {
            schema: wire.schema,
            module_id: wire.module_id,
            origin: ModuleOrigin::parse(&wire.origin).map_err(D::Error::custom)?,
            trust_class: parse_trust_class(&wire.trust_class).map_err(D::Error::custom)?,
            scope: wire.scope,
            deps: wire.deps,
            capabilities: wire.capabilities,
            source: wire.source,
            state_schema: wire.state_schema,
            state_key: wire.state_key,
        })
    }
}

fn deserialize_capability_vec<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<Capability>, D::Error> {
    #[derive(Deserialize)]
    struct Wire {
        resource: String,
        verbs: Vec<String>,
    }
    let wire: Vec<Wire> = Vec::deserialize(de)?;
    Ok(wire
        .into_iter()
        .map(|w| Capability::new(w.resource, w.verbs))
        .collect())
}

/// The content-addressed store module packages live in (decision 33/T14).
///
/// The primary store is where installs land: the global `<state>/modules`
/// store when the session runs under a layout, else the session's own
/// `objects/`. The optional fallback is READ-ONLY resolution for packages that
/// predate the global store — a layout session whose packages were installed
/// into its session store (a legacy dir a migration copied in place) keeps
/// resolving them instead of failing as missing. Reads hash-verify, so a
/// fallback hit is byte-identical to a primary hit; a write never touches the
/// fallback.
pub struct PackageStore {
    primary: ObjectStore,
    fallback: Option<ObjectStore>,
}

impl PackageStore {
    /// A layered store: installs go to `primary`, reads fall back to
    /// `fallback` when the primary is missing the digest.
    pub fn with_fallback(primary: ObjectStore, fallback: ObjectStore) -> Self {
        Self {
            primary,
            fallback: Some(fallback),
        }
    }

    /// The bytes of `want` — the primary store, else the fallback. Verified
    /// either way; only `Missing` falls through, so a corrupt primary object
    /// stays a corruption error instead of being masked by a fallback copy.
    pub fn get(&self, want: &Digest) -> Result<Vec<u8>, ObjectError> {
        match self.primary.get(want) {
            Err(ObjectError::Missing { .. }) => match &self.fallback {
                Some(fallback) => fallback.get(want),
                None => Err(ObjectError::Missing { digest: *want }),
            },
            other => other,
        }
    }

    /// Whether `digest` resolves in either layer.
    pub fn exists(&self, digest: &Digest) -> bool {
        self.primary.exists(digest) || self.fallback.as_ref().is_some_and(|f| f.exists(digest))
    }

    /// Installs into the primary store (content-deduped there).
    pub fn install(&mut self, bytes: &[u8]) -> io::Result<Digest> {
        self.primary.install(bytes)
    }
}

impl From<ObjectStore> for PackageStore {
    fn from(primary: ObjectStore) -> Self {
        Self {
            primary,
            fallback: None,
        }
    }
}

/// Installs the manifest's canonical JSON as a package object (content-deduped).
/// Returns `(package digest, deduped)` where `deduped` = the package already
/// resolved (a fallback hit counts — the same bytes are already readable); the
/// primary store is materialized either way.
pub fn install_package(
    store: &mut PackageStore,
    manifest: &PackageManifest,
) -> Result<(Digest, bool), PackageError> {
    if manifest.schema != PACKAGE_SCHEMA {
        return Err(PackageError::SchemaMismatch {
            expected: PACKAGE_SCHEMA,
            actual: manifest.schema,
        });
    }
    let bytes = serde_json::to_vec(manifest)
        .map_err(|e| PackageError::InvalidInput(format!("manifest is not canonical JSON: {e}")))?;
    let digest = Digest::new(&bytes);
    let deduped = store.exists(&digest);
    store.install(&bytes)?;
    Ok((digest, deduped))
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("package schema {actual} is not supported (expected {expected})")]
    SchemaMismatch { expected: u32, actual: u32 },
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::queue::DurabilityQueue;
    use std::sync::Arc;

    /// A raw primary/fallback store to layer (the bytes go in verbatim).
    fn store(tag: &str) -> (std::path::PathBuf, ObjectStore, Arc<DurabilityQueue>) {
        let dir = std::env::temp_dir().join(format!(
            "kb-package-store-{tag}-{}-{}",
            std::process::id(),
            Id128::generate()
        ));
        let queue = Arc::new(DurabilityQueue::start(&format!("test-package-{tag}")));
        let store = ObjectStore::open(&dir, Arc::clone(&queue)).unwrap();
        (dir, store, queue)
    }

    fn cleanup(dir: std::path::PathBuf, queue: Arc<DurabilityQueue>) {
        let _ = std::fs::remove_dir_all(dir);
        if let Ok(queue) = Arc::try_unwrap(queue) {
            let _ = queue.shutdown();
        }
    }

    /// A layered store writes to the primary and resolves a fallback-only
    /// digest (decision 33/T14): the legacy session-store copy of a package
    /// installed before the global store existed stays readable.
    #[test]
    fn layered_store_installs_to_primary_and_reads_the_fallback() {
        let (primary_dir, primary, primary_queue) = store("layered-primary");
        let (fallback_dir, mut fallback, fallback_queue) = store("layered-fallback");
        let legacy = fallback.install(b"{\"legacy package\":1}").unwrap();
        let mut layered = PackageStore::with_fallback(primary, fallback);
        assert!(layered.exists(&legacy), "the fallback digest resolves");
        assert_eq!(layered.get(&legacy).unwrap(), b"{\"legacy package\":1}");

        let fresh = layered.install(b"{\"fresh package\":1}").unwrap();
        assert!(primary_dir.join(fresh.to_string()).is_file());
        assert!(
            !fallback_dir.join(fresh.to_string()).exists(),
            "an install never writes through to the fallback"
        );
        cleanup(primary_dir, primary_queue);
        cleanup(fallback_dir, fallback_queue);
    }

    /// A corrupt primary object is a corruption error, never masked by a
    /// healthy fallback copy (the digest identity is the contract).
    #[test]
    fn corrupt_primary_is_not_masked_by_the_fallback() {
        let (primary_dir, mut primary, primary_queue) = store("corrupt-primary");
        let (fallback_dir, mut fallback, fallback_queue) = store("corrupt-fallback");
        let bytes = b"{\"package\":1}";
        let digest = primary.install(bytes).unwrap();
        fallback.install(bytes).unwrap();
        std::fs::write(primary_dir.join(digest.to_string()), b"garbage").unwrap();
        let layered = PackageStore::with_fallback(primary, fallback);
        assert!(matches!(
            layered.get(&digest),
            Err(ObjectError::Corruption { .. })
        ));
        cleanup(primary_dir, primary_queue);
        cleanup(fallback_dir, fallback_queue);
    }

    /// A missing digest in both layers is `Missing`, not an empty read.
    #[test]
    fn missing_in_both_layers_is_missing() {
        let (primary_dir, primary, primary_queue) = store("missing-primary");
        let (fallback_dir, fallback, fallback_queue) = store("missing-fallback");
        let layered = PackageStore::with_fallback(primary, fallback);
        let want = Digest::new(b"never installed");
        assert!(matches!(
            layered.get(&want),
            Err(ObjectError::Missing { digest }) if digest == want
        ));
        cleanup(primary_dir, primary_queue);
        cleanup(fallback_dir, fallback_queue);
    }

    #[test]
    fn wire_names_roundtrip() {
        let m = PackageManifest {
            schema: PACKAGE_SCHEMA,
            module_id: Id128::generate(),
            origin: ModuleOrigin::Agent,
            trust_class: TrustClass::Workspace,
            scope: ScopePath(vec!["root".into(), "child".into()]),
            deps: vec![ServiceDependency {
                key: kanbei_services::ServiceKey {
                    scope: ScopePath(vec!["root".into()]),
                    name: "svc".into(),
                },
                required_version: 2,
            }],
            capabilities: vec![Capability::new("fs.read".into(), vec!["read".into()])],
            source: "function kb_hot(x) return x end".into(),
            state_schema: Some(1),
            state_key: Some("planner".into()),
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: PackageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, m);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["origin"], "agent");
        assert_eq!(v["trust_class"], "workspace");
    }
}
