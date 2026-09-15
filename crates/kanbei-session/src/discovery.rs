//! Config discovery (decision 28/T8): resolves the desired-state config layers
//! the shipped CLI activates at startup, in LOW→HIGH precedence.
//!
//! Layers: (a) always the built-in defaults ([`builtin_config_manifest`]),
//! (b) the user config at `$XDG_CONFIG_HOME/kanbei/init.lua` (falling back to
//! `$HOME/.config/kanbei/init.lua`), then (c) the project config at
//! `<project_dir>/.kanbei/init.lua`. A layer whose file is ABSENT is skipped;
//! a layer whose file EXISTS but cannot be read is an error, which the caller
//! treats as invalid config and degrades to built-ins (safe mode).
//!
//! Each file-backed layer gets a deterministic module id derived from its
//! source text plus an origin discriminator, so identical files in the user
//! and project layers stay distinct and repeated runs address the same
//! identity (R-08 stable ModuleId + immutable content hash).

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kanbei_capabilities::TrustClass;
use kanbei_modules::{ModuleOrigin, PACKAGE_SCHEMA, PackageManifest};
use thiserror::Error;

use crate::builtin_config::{builtin_config_manifest, config_module_id, root_scope};

/// A discovered config layer failed to load. An absent file is not an error;
/// an existing-but-unreadable one is (the caller degrades to built-ins).
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("cannot read config layer {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Discovers the desired-state config layers for `project_dir`, ordered
/// LOW→HIGH: built-in defaults, user config, project config. A layer whose
/// file is absent is skipped; an unreadable file is an [`DiscoveryError`].
pub fn discover_config_layers(project_dir: &Path) -> Result<Vec<PackageManifest>, DiscoveryError> {
    discover_config_layers_with(
        project_dir,
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

/// Env-explicit form of [`discover_config_layers`] (unit-testable without
/// mutating the process environment).
fn discover_config_layers_with(
    project_dir: &Path,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Vec<PackageManifest>, DiscoveryError> {
    let mut layers = vec![builtin_config_manifest()];
    if let Some(path) = user_config_path(xdg_config_home, home)
        && let Some(manifest) = load_layer(&path, ModuleOrigin::UserConfig, TrustClass::User)?
    {
        layers.push(manifest);
    }
    let project_path = project_dir.join(".kanbei").join("init.lua");
    if let Some(manifest) = load_layer(
        &project_path,
        ModuleOrigin::WorkspaceConfig,
        TrustClass::Workspace,
    )? {
        layers.push(manifest);
    }
    Ok(layers)
}

/// `$XDG_CONFIG_HOME/kanbei/init.lua`, falling back to
/// `$HOME/.config/kanbei/init.lua`. None when neither is set (empty values
/// count as unset, matching the XDG base-directory spec).
fn user_config_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        let xdg = PathBuf::from(xdg);
        // The XDG base-directory spec requires an absolute path; a relative
        // one is treated as unset (fall through to $HOME).
        if xdg.is_absolute() {
            return Some(xdg.join("kanbei").join("init.lua"));
        }
    }
    let home = home.filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("kanbei")
            .join("init.lua"),
    )
}

/// Reads one optional config layer. Absent → `Ok(None)`; any other read
/// failure → `Err` (invalid config).
fn load_layer(
    path: &Path,
    origin: ModuleOrigin,
    trust_class: TrustClass,
) -> Result<Option<PackageManifest>, DiscoveryError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DiscoveryError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    Ok(Some(PackageManifest {
        schema: PACKAGE_SCHEMA,
        module_id: layer_module_id(&source, origin),
        origin,
        trust_class,
        scope: root_scope(),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source,
        state_schema: None,
        state_key: None,
    }))
}

/// Deterministic module id for a file-backed layer: the source text prefixed
/// with an origin discriminator, so identical files in the user and project
/// layers do not collide.
fn layer_module_id(source: &str, origin: ModuleOrigin) -> kanbei_core::id::Id128 {
    let discriminator: &[u8] = match origin {
        ModuleOrigin::WorkspaceConfig => b"workspace_config",
        _ => b"user_config",
    };
    let mut seed = Vec::with_capacity(discriminator.len() + 1 + source.len());
    seed.extend_from_slice(discriminator);
    seed.push(0);
    seed.extend_from_slice(source.as_bytes());
    config_module_id(&seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::Digest;

    fn package_digest(m: &PackageManifest) -> Digest {
        Digest::new(&serde_json::to_vec(m).expect("manifest serialization cannot fail"))
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kb-discovery-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn user_init(config_home: &Path) -> PathBuf {
        config_home.join("kanbei").join("init.lua")
    }

    /// No user or project file → exactly the built-in layer.
    #[test]
    fn no_files_yields_only_builtin() {
        let dir = TempDir::new("empty");
        let cfg = TempDir::new("empty-cfg");
        let layers =
            discover_config_layers_with(dir.path(), Some(cfg.path().as_os_str()), None).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], builtin_config_manifest());
    }

    /// A user file present → built-in then user, in that order.
    #[test]
    fn user_file_yields_builtin_then_user() {
        let dir = TempDir::new("user");
        let cfg = TempDir::new("user-cfg");
        let src = "function kb_hot(x) return x end\n";
        write(&user_init(cfg.path()), src);
        let layers = discover_config_layers_with(
            dir.path(),
            Some(cfg.path().as_os_str()),
            None,
        )
        .unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].origin, ModuleOrigin::Builtin);
        assert_eq!(layers[1].origin, ModuleOrigin::UserConfig);
        assert_eq!(layers[1].trust_class, TrustClass::User);
        assert_eq!(layers[1].scope, root_scope());
        assert_eq!(layers[1].schema, PACKAGE_SCHEMA);
        assert_eq!(layers[1].source, src);
        assert!(layers[1].deps.is_empty());
        assert!(layers[1].capabilities.is_empty());
        assert_eq!(layers[1].state_schema, None);
        assert_eq!(layers[1].state_key, None);
    }

    /// Both files present → built-in, user, project, in that order.
    #[test]
    fn project_file_yields_ordered_layers() {
        let dir = TempDir::new("project");
        let cfg = TempDir::new("project-cfg");
        write(&user_init(cfg.path()), "user");
        write(&dir.path().join(".kanbei").join("init.lua"), "project");
        let layers =
            discover_config_layers_with(dir.path(), Some(cfg.path().as_os_str()), None).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].origin, ModuleOrigin::Builtin);
        assert_eq!(layers[1].origin, ModuleOrigin::UserConfig);
        assert_eq!(layers[2].origin, ModuleOrigin::WorkspaceConfig);
        assert_eq!(layers[2].trust_class, TrustClass::Workspace);
        assert_eq!(layers[2].source, "project");
    }

    /// The XDG fallback is `$HOME/.config/kanbei/init.lua` when
    /// `XDG_CONFIG_HOME` is unset.
    #[test]
    fn home_fallback_path() {
        let home = TempDir::new("home");
        let dir = TempDir::new("home-proj");
        write(
            &home.path().join(".config").join("kanbei").join("init.lua"),
            "home",
        );
        let layers =
            discover_config_layers_with(dir.path(), None, Some(home.path().as_os_str())).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].source, "home");
    }

    /// Repeated discovery of the same files yields identical module ids and
    /// package digests; identical user/project source stays distinct.
    #[test]
    fn ids_are_deterministic_and_origin_distinct() {
        let dir = TempDir::new("det");
        let cfg = TempDir::new("det-cfg");
        write(&user_init(cfg.path()), "same");
        write(&dir.path().join(".kanbei").join("init.lua"), "same");
        let a =
            discover_config_layers_with(dir.path(), Some(cfg.path().as_os_str()), None).unwrap();
        let b =
            discover_config_layers_with(dir.path(), Some(cfg.path().as_os_str()), None).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].module_id, b[1].module_id);
        assert_eq!(a[2].module_id, b[2].module_id);
        assert_eq!(package_digest(&a[1]), package_digest(&b[1]));
        assert_eq!(package_digest(&a[2]), package_digest(&b[2]));
        assert_ne!(
            a[1].module_id, a[2].module_id,
            "identical source in user and project layers must stay distinct"
        );
    }

    /// An existing-but-unreadable file is an error, not a skipped layer.
    #[test]
    fn unreadable_file_is_an_error() {
        let dir = TempDir::new("unreadable");
        let cfg = TempDir::new("unreadable-cfg");
        // A directory where the file is expected: read_to_string fails with
        // something other than NotFound.
        fs::create_dir_all(user_init(cfg.path())).unwrap();
        let err =
            discover_config_layers_with(dir.path(), Some(cfg.path().as_os_str()), None).unwrap_err();
        assert!(matches!(err, DiscoveryError::Io { .. }), "{err:?}");
    }

    /// Absent HOME and XDG do not panic and skip the user layer.
    #[test]
    fn absent_home_and_xdg_does_not_panic() {
        let dir = TempDir::new("no-home");
        let layers = discover_config_layers_with(dir.path(), None, None).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], builtin_config_manifest());
    }

    /// A relative `XDG_CONFIG_HOME` violates the spec and is treated as unset:
    /// the user layer falls back to `$HOME/.config`.
    #[test]
    fn relative_xdg_config_home_is_treated_as_unset() {
        let home = TempDir::new("rel-xdg-home");
        let dir = TempDir::new("rel-xdg-proj");
        write(
            &home.path().join(".config").join("kanbei").join("init.lua"),
            "home-layer",
        );
        let layers = discover_config_layers_with(
            dir.path(),
            Some(OsStr::new("relative/config")),
            Some(home.path().as_os_str()),
        )
        .unwrap();
        assert_eq!(layers.len(), 2, "the HOME fallback layer is used");
        assert_eq!(layers[1].source, "home-layer");
    }
}
