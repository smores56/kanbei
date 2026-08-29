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

/// Package manifest schema version (M2: 1).
pub const PACKAGE_SCHEMA: u32 = 1;

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
        let mut st = ser.serialize_struct("PackageManifest", 9)?;
        st.serialize_field("schema", &self.schema)?;
        st.serialize_field("module_id", &self.module_id)?;
        st.serialize_field("origin", &self.origin.name())?;
        st.serialize_field("trust_class", &trust_class_name(self.trust_class))?;
        st.serialize_field("scope", &self.scope)?;
        st.serialize_field("deps", &self.deps)?;
        st.serialize_field("capabilities", &capabilities)?;
        st.serialize_field("source", &self.source)?;
        st.serialize_field("state_schema", &self.state_schema)?;
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

/// Installs the manifest's canonical JSON as a package object (content-deduped).
/// Returns `(package digest, deduped)` where `deduped` = the object already
/// existed.
pub fn install_package(
    store: &mut ObjectStore,
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
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: PackageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, m);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["origin"], "agent");
        assert_eq!(v["trust_class"], "workspace");
    }
}
