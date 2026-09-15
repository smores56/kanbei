//! T8 settings seam (decision 28): `SessionConfig.settings` resolves the
//! running session's wiring from the merged config-layer settings. A `Some`
//! `SessionSettings` field overrides the corresponding `SessionConfig` value;
//! `None` (or no source at all) keeps today's argv/env-derived behavior.
//!
//! A missing guest is a hard failure: build it with `cargo xtask build-guest`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::{ModuleOrigin, PackageManifest};
use kanbei_provider::{FakeEngine, KeySource, ProviderConfig, ProviderEngine};
use kanbei_scopes::contrib::SettingsContribution;
use kanbei_session::{
    Session, SessionConfig, SessionSettings, SettingsSource, builtin_config_manifest,
};
use kanbei_snapshot::ExecutionManifest;
use kanbei_vm::{GuestError, Vm, VmConfig};

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-settings-{tag}-{}-{}",
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

fn require_guest() {
    match Vm::load(no_epoch()) {
        Ok(_) => {}
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

/// A settings-only config layer: `kb_on_activate` publishes one typed settings
/// contribution (the shape the shipped Luau layers use).
fn settings_manifest(origin: ModuleOrigin, trust_class: TrustClass, payload: &str) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin,
        trust_class,
        scope: kanbei_session::root_scope(),
        deps: vec![],
        capabilities: vec![],
        source: format!(
            "function kb_on_activate(ctx) ctx.contribution_publish('{payload}') end\nfunction kb_hot(x) return x end"
        ),
        state_schema: None,
        state_key: None,
    }
}

fn cfg(base_url: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        provider: "cfg".into(),
        model: model.into(),
        base_url: base_url.into(),
        key: KeySource::Env("KANBEI_PROVIDER_KEY".into()),
        temperature: None,
        max_tokens: None,
        timeout: std::time::Duration::from_secs(5),
    }
}

fn yolo_like_broker(session_id: Id128) -> Broker {
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![Capability::new("cfg-tool".into(), vec!["call".into()])],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: Digest::new(b"placeholder"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: Capability::new("cfg-tool".into(), vec!["call".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("settings test".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    broker
}

/// Builds the runtime wiring *from the merged settings*: a provider config is
/// resolved only once a layer supplies a base URL (so the built-in layer alone
/// yields no engine), mirroring the CLI source. Returns a fixed session id +
/// broker so the test can assert the overrides landed.
struct ConfigSource {
    id: Id128,
}

impl SettingsSource for ConfigSource {
    fn resolve(&self, settings: &SettingsContribution) -> SessionSettings {
        let Some(provider) = settings.provider.as_ref() else {
            return SessionSettings::default();
        };
        let Some(base_url) = provider.base_url.clone() else {
            return SessionSettings::default();
        };
        let model = provider.model.clone().unwrap_or_else(|| "cfg-model".into());
        let config = cfg(&base_url, &model);
        let engine: Box<dyn ProviderEngine> = Box::new(FakeEngine::new(config.clone(), vec![]));
        SessionSettings {
            provider_engine: Some(engine),
            provider: Some(config),
            broker: Some(yolo_like_broker(self.id)),
            approval_resolver: None,
            session_id: Some(self.id),
        }
    }
}

/// A source that resolves to nothing — exercise the fallback-to-SessionConfig
/// path.
struct EmptySource;

impl SettingsSource for EmptySource {
    fn resolve(&self, _settings: &SettingsContribution) -> SessionSettings {
        SessionSettings::default()
    }
}

// --- tests -----------------------------------------------------------------

/// The resolved `Some` fields override `SessionConfig`'s engine, broker, and
/// session id after config activation.
#[test]
fn settings_source_overrides_session_wiring() {
    require_guest();
    let dir = TempDir::new("override");
    let id = Id128::generate();
    let project = settings_manifest(
        ModuleOrigin::WorkspaceConfig,
        TrustClass::Workspace,
        r#"{"kind":"settings","provider":{"base_url":"https://cfg","model":"cfg-model"},"approval":{"auto_approve":true}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), project],
        settings: Some(Arc::new(ConfigSource { id })),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        session.session_id(),
        id,
        "settings session_id overrides the generated one"
    );
    assert_eq!(
        session.provider_engine().expect("engine resolved").identity(),
        "cfg",
        "settings provider config drives the engine"
    );
    let principal = Principal {
        session: id,
        generation: 0,
        run: None,
    };
    session
        .broker()
        .check(
            &principal,
            &Capability::new("cfg-tool".into(), vec!["call".into()]),
            1,
        )
        .expect("settings broker grants cfg-tool to the resolved session id");
    session.close().unwrap();
}

/// The settings-resolved provider config is pinned as a content digest in the
/// post-event manifest of the composition event (ordering: resolve before the
/// canonical commit).
#[test]
fn settings_provider_config_pins_at_composition_event() {
    require_guest();
    let dir = TempDir::new("pin");
    let config = cfg("https://cfg", "cfg-model");
    let expected = Digest::new(&config.to_canonical_bytes());
    let project = settings_manifest(
        ModuleOrigin::WorkspaceConfig,
        TrustClass::Workspace,
        r#"{"kind":"settings","provider":{"base_url":"https://cfg","model":"cfg-model"}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), project],
        settings: Some(Arc::new(ConfigSource {
            id: Id128::generate(),
        })),
        ..Default::default()
    })
    .unwrap();
    let snap = session
        .current_snapshot()
        .expect("the composition commit pinned a manifest");
    let bytes = session.store().get(&snap).unwrap();
    let manifest = ExecutionManifest::from_bytes(&bytes).unwrap();
    assert_eq!(
        manifest.provider_config,
        Some(expected),
        "the config event's post-manifest pins the settings-resolved provider config"
    );
    session.close().unwrap();
}

/// No settings source keeps today's behavior: the injected engine is used
/// untouched.
#[test]
fn absent_settings_source_preserves_injected_engine() {
    let dir = TempDir::new("none");
    let engine: Box<dyn ProviderEngine> = Box::new(FakeEngine::new(cfg("https://x", "injected"), vec![]));
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider_engine: Some(engine),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        session.provider_engine().expect("engine bound").identity(),
        "cfg"
    );
    session.close().unwrap();
}

/// A source whose every field is `None` falls back to the `SessionConfig`
/// values, not to the settings.
#[test]
fn empty_settings_resolution_falls_back_to_session_config() {
    require_guest();
    let dir = TempDir::new("fallback");
    let id = Id128::generate();
    let injected: Box<dyn ProviderEngine> = Box::new(FakeEngine::new(cfg("https://x", "injected"), vec![]));
    let project = settings_manifest(
        ModuleOrigin::WorkspaceConfig,
        TrustClass::Workspace,
        r#"{"kind":"settings","provider":{"base_url":"https://cfg"}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), project],
        provider_engine: Some(injected),
        session_id: Some(id),
        settings: Some(Arc::new(EmptySource)),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(session.session_id(), id, "fallback keeps the configured id");
    assert_eq!(
        session.provider_engine().expect("engine bound").identity(),
        "cfg",
        "fallback keeps the injected engine"
    );
    session.close().unwrap();
}
