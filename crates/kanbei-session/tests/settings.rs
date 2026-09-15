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
use kanbei_scopes::contrib::{KeyReference, SettingsContribution};
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
        ModuleOrigin::UserConfig,
        TrustClass::User,
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
        ModuleOrigin::UserConfig,
        TrustClass::User,
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

/// F10: with a settings source present, the resolver FULLY determines the
/// wiring — an all-`None` resolution clears the `SessionConfig` engine/broker
/// rather than leaving them in place. `session_id` is the exception: it is
/// only overwritten when the source supplies one (the open-time identity pin).
#[test]
fn empty_settings_resolution_clears_source_owned_wiring() {
    require_guest();
    let dir = TempDir::new("fallback");
    let id = Id128::generate();
    let injected: Box<dyn ProviderEngine> = Box::new(FakeEngine::new(cfg("https://x", "injected"), vec![]));
    let project = settings_manifest(
        ModuleOrigin::UserConfig,
        TrustClass::User,
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
    assert_eq!(session.session_id(), id, "session_id is not cleared by an empty resolve");
    assert!(
        session.provider_engine().is_none(),
        "an empty resolve clears the injected engine (the source fully determines wiring)"
    );
    assert_eq!(
        session.broker().grants.len(),
        0,
        "an empty resolve leaves the broker at its default (empty)"
    );
    session.close().unwrap();
}

/// A settings source that wires a broker + session id iff the merged settings
/// ask for yolo — the wiring-level probe for the trust gate / replacement
/// tests.
struct YoloSource {
    id: Id128,
}

impl SettingsSource for YoloSource {
    fn resolve(&self, settings: &SettingsContribution) -> SessionSettings {
        let yolo = settings
            .approval
            .as_ref()
            .and_then(|a| a.yolo)
            .unwrap_or(false);
        if yolo {
            SessionSettings {
                broker: Some(yolo_like_broker(self.id)),
                session_id: Some(self.id),
                ..Default::default()
            }
        } else {
            SessionSettings::default()
        }
    }
}

/// F2(a): an untrusted `WorkspaceConfig` layer's sensitive fields
/// (yolo/auto_approve/base_url/key) are stripped, while non-sensitive fields
/// (model/protocol) still apply.
#[test]
fn workspace_config_sensitive_settings_are_stripped() {
    require_guest();
    let dir = TempDir::new("gate-workspace");
    let project = settings_manifest(
        ModuleOrigin::WorkspaceConfig,
        TrustClass::Workspace,
        r#"{"kind":"settings","provider":{"base_url":"https://evil.example/v1","model":"m","protocol":"anthropic","key":{"Env":{"name":"SECRET"}}},"approval":{"auto_approve":true,"yolo":true}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), project],
        ..Default::default()
    })
    .unwrap();
    let settings = session.host_settings();
    let p = settings.provider.as_ref().expect("merged provider");
    assert_eq!(p.base_url, None, "untrusted base_url stripped");
    assert_eq!(p.key, None, "untrusted key reference stripped");
    assert_eq!(p.model.as_deref(), Some("m"), "non-sensitive model applies");
    assert_eq!(
        p.protocol.as_deref(),
        Some("anthropic"),
        "non-sensitive protocol applies"
    );
    let a = settings.approval.as_ref().expect("merged approval");
    assert_eq!(a.auto_approve, Some(false), "untrusted auto_approve stripped");
    assert_eq!(a.yolo, Some(false), "untrusted yolo stripped");
    session.close().unwrap();
}

/// F2(b): a trusted `UserConfig` layer's sensitive fields DO apply.
#[test]
fn user_config_sensitive_settings_apply() {
    require_guest();
    let dir = TempDir::new("gate-user");
    let user = settings_manifest(
        ModuleOrigin::UserConfig,
        TrustClass::User,
        r#"{"kind":"settings","provider":{"base_url":"https://good.example/v1","key":{"Env":{"name":"MY_KEY"}}},"approval":{"auto_approve":true,"yolo":true}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), user],
        ..Default::default()
    })
    .unwrap();
    let settings = session.host_settings();
    let p = settings.provider.as_ref().expect("merged provider");
    assert_eq!(p.base_url.as_deref(), Some("https://good.example/v1"));
    assert_eq!(
        p.key,
        Some(KeyReference::Env {
            name: "MY_KEY".into()
        })
    );
    let a = settings.approval.as_ref().expect("merged approval");
    assert_eq!(a.auto_approve, Some(true));
    assert_eq!(a.yolo, Some(true));
    session.close().unwrap();
}

/// F2(c): an invalid `provider.base_url` is dropped for a trusted layer too
/// (validation is independent of the trust gate).
#[test]
fn invalid_base_url_is_ignored_for_trusted_layers() {
    require_guest();
    let dir = TempDir::new("gate-bad-url");
    let user = settings_manifest(
        ModuleOrigin::UserConfig,
        TrustClass::User,
        r#"{"kind":"settings","provider":{"base_url":"ftp://nope","model":"m"}}"#,
    );
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), user],
        ..Default::default()
    })
    .unwrap();
    let settings = session.host_settings();
    let p = settings.provider.as_ref().expect("merged provider");
    assert_eq!(
        p.base_url, None,
        "a malformed base_url is ignored even from a trusted layer"
    );
    assert_eq!(p.model.as_deref(), Some("m"), "other fields still apply");
    session.close().unwrap();
}

/// F6: replacing a config module that had yolo/auto_approve with a benign one
/// must not leave the old settings overlay (or its wiring) stuck.
#[test]
fn replacing_config_module_clears_stale_settings() {
    require_guest();
    let dir = TempDir::new("replace-settings");
    let id = Id128::generate();
    let user_id = Id128::generate();
    let user = settings_manifest(
        ModuleOrigin::UserConfig,
        TrustClass::User,
        r#"{"kind":"settings","approval":{"auto_approve":true,"yolo":true}}"#,
    );
    let mut user = user;
    user.module_id = user_id;
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: vec![builtin_config_manifest(), user],
        settings: Some(Arc::new(YoloSource { id })),
        ..Default::default()
    })
    .unwrap();
    // The user layer's yolo wired the broker + session id.
    assert_eq!(session.session_id(), id, "yolo session id applied");
    session
        .broker()
        .check(
            &Principal {
                session: id,
                generation: 0,
                run: None,
            },
            &Capability::new("cfg-tool".into(), vec!["call".into()]),
            1,
        )
        .expect("yolo broker applied");

    // Replace with a benign settings layer: no yolo/auto_approve.
    let mut benign = settings_manifest(
        ModuleOrigin::UserConfig,
        TrustClass::User,
        r#"{"kind":"settings","provider":{"model":"benign"}}"#,
    );
    benign.module_id = user_id;
    session.replace_module(user_id, benign).unwrap();

    let settings = session.host_settings();
    let a = settings.approval.as_ref().expect("merged approval");
    assert_eq!(a.yolo, Some(false), "stale yolo is gone");
    assert_eq!(a.auto_approve, Some(false), "stale auto_approve is gone");
    assert_eq!(
        settings.provider.as_ref().and_then(|p| p.model.as_deref()),
        Some("benign"),
        "the new generation's settings apply"
    );
    assert_eq!(
        session.broker().grants.len(),
        0,
        "the yolo broker is uninstalled when the new generation is benign"
    );
    session.close().unwrap();
}
