//! The kernel's built-in desired-state config layer (decision 28): the lowest
//! precedence Luau `init.lua` generation. It publishes only default settings,
//! so every higher layer (user, project) overrides just the fields it owns via
//! the registry's field-wise settings overlay.
//!
//! This generation is immutable content: its module id and package digest are
//! derived from the source, so rebuilds/reopens address the same identity
//! (R-08 stable ModuleId + immutable content hash). The built-in UI generation
//! derives its id the same way ([`crate::builtin_ui_module_id`]).

use kanbei_capabilities::TrustClass;
use kanbei_core::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::{ModuleOrigin, PACKAGE_SCHEMA, PackageManifest};
use kanbei_services::ScopePath;

/// The built-in config source: a minimal Luau module defining `kb_hot` and
/// `kb_on_activate(ctx)` (the guest contract) that publishes the default
/// settings contribution. Top-level code is pure (runs twice).
pub const BUILTIN_CONFIG_SOURCE: &str = r#"-- kanbei built-in config defaults (decision 28).
-- Lowest-precedence desired-state layer: publishes default settings and the
-- built-in keybinding layer (decision 29).
function kb_on_activate(ctx)
  ctx.contribution_publish(
    '{"kind":"settings",' ..
    '"provider":{"protocol":"openai"},' ..
    '"approval":{"auto_approve":false,"yolo":false}}')
  -- Decision 29: the built-in layer ships the kernel's default bindings so
  -- making Ctrl-C/Ctrl-Q/Ctrl-L remappable does not drop run cancellation,
  -- quit, or repaint out of the box. Origin is kernel-stamped as `builtin`.
  -- The approval gate is a modal context (decision 8): y/n decide the parked
  -- approval through the same keymap path as every key, and Ctrl-C denies it
  -- (the modal deny outranks the always cancel_run), so no ad-hoc key mapping
  -- is needed.
  ctx.contribution_publish(
    '{"kind":"keymap","bindings":[' ..
    '{"key":"ctrl-c","context":"always","action":"cancel_run"},' ..
    '{"key":"ctrl-q","context":"always","action":"quit"},' ..
    '{"key":"ctrl-l","context":"always","action":"repaint"},' ..
    '{"key":"y","context":"modal","action":"approve"},' ..
    '{"key":"Y","context":"modal","action":"approve"},' ..
    '{"key":"n","context":"modal","action":"deny"},' ..
    '{"key":"N","context":"modal","action":"deny"},' ..
    '{"key":"ctrl-c","context":"modal","action":"deny"}]}')
end

function kb_hot(dispatch)
  return dispatch
end
"#;

/// The canonical root scope every config layer contributes to (the empty path,
/// as [`kanbei_scopes::ScopeTree::new_root`] defines it).
pub fn root_scope() -> ScopePath {
    ScopePath(vec![])
}

/// A deterministic module id for immutable config content: the first 16 bytes
/// of the seed digest, shaped as a UUIDv7 (nonzero high byte with the top bit
/// clear, version/variant nibbles) so the text form keeps the frozen 21-char
/// base58 width. Shared by the built-in layer and file-discovered layers
/// ([`crate::discovery`]) so rebuilds/reopens address the same identity.
pub(crate) fn config_module_id(seed: &[u8]) -> Id128 {
    let digest = Digest::new(seed);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    // Freeze the text form at the 21-char base58 width: 16 bytes need a high
    // byte ≤ 0x07 (else 22 chars) and nonzero (else a leading '1' pad), plus
    // UUIDv7 version/variant nibbles.
    bytes[0] = (bytes[0] & 0x07) | 0x01;
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Id128::from_bytes(bytes)
}

/// The built-in layer's deterministic module id, derived from its source.
pub fn builtin_config_module_id() -> Id128 {
    config_module_id(BUILTIN_CONFIG_SOURCE.as_bytes())
}

/// The built-in config generation manifest: `Builtin` origin/trust, root
/// scope, current package schema. Deterministic — two calls produce the same
/// module id and the same package digest.
pub fn builtin_config_manifest() -> PackageManifest {
    PackageManifest {
        schema: PACKAGE_SCHEMA,
        module_id: builtin_config_module_id(),
        origin: ModuleOrigin::Builtin,
        trust_class: TrustClass::Builtin,
        scope: root_scope(),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: BUILTIN_CONFIG_SOURCE.to_string(),
        state_schema: None,
        state_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_digest(m: &PackageManifest) -> Digest {
        Digest::new(&serde_json::to_vec(m).expect("manifest serialization cannot fail"))
    }

    /// Two calls produce an identical module id AND an identical package
    /// digest (the identity a resolution/GC layer addresses).
    #[test]
    fn builtin_config_manifest_is_deterministic() {
        let a = builtin_config_manifest();
        let b = builtin_config_manifest();
        assert_eq!(a.module_id, b.module_id, "module id must be stable");
        assert_eq!(
            package_digest(&a),
            package_digest(&b),
            "package digest must be stable"
        );
        assert_eq!(a.module_id, builtin_config_module_id());
    }

    /// The text form keeps the frozen 21-char base58 width.
    #[test]
    fn builtin_config_module_id_text_is_a_stable_width() {
        assert_eq!(builtin_config_module_id().to_string().len(), 21);
    }

    /// The generation is kernel-trusted, built-in, and root-scoped.
    #[test]
    fn builtin_config_manifest_is_builtin_root_scope() {
        let m = builtin_config_manifest();
        assert_eq!(m.schema, PACKAGE_SCHEMA);
        assert_eq!(m.origin, ModuleOrigin::Builtin);
        assert_eq!(m.trust_class, TrustClass::Builtin);
        assert_eq!(m.scope, root_scope());
        assert!(m.deps.is_empty());
        assert!(m.source.contains("kb_on_activate"));
        assert!(m.source.contains("kb_hot"));
        assert!(m.source.contains(r#""kind":"settings""#));
        assert!(m.source.contains(r#""kind":"keymap""#), "built-in defaults ship bindings");
        assert!(m.source.contains(r#""action":"cancel_run""#));
        assert!(m.source.contains(r#""action":"approve""#));
        assert!(m.source.contains(r#""action":"deny""#));
        assert!(m.source.contains(r#""context":"modal""#), "approval keys are modal");
    }
}
