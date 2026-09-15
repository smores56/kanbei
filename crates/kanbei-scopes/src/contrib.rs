//! Typed structural contributions (R-19/A-11/C): modules contribute typed
//! entries, never resolution logic; the kernel owns the fixed per-type
//! conflict rules.

use kanbei_core::id::Id128;
use kanbei_services::{ScopePath, ServiceDependency, ServiceKey, ServiceProvider};
use serde::{Deserialize, Serialize};

/// A typed contribution bound to a scope.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    pub scope: ScopePath,
    pub kind: ContributionKind,
}

/// The typed contribution kinds; one per structural domain registry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ContributionKind {
    Command(CommandContribution),
    Tool(ToolContribution),
    Service(ServiceContribution),
    Keymap(Keybinding),
    Theme(ThemeContribution),
    ProjectionStage(ProjectionStageContribution),
    UiMount(UiMountContribution),
    Guard(GuardContribution),
    Settings(SettingsContribution),
    Hook(HookContribution),
}

impl ContributionKind {
    /// Stable kind tag used for conflict classification and snapshot ordering.
    pub(crate) fn kind_tag(&self) -> &'static str {
        match self {
            ContributionKind::Command(_) => "command",
            ContributionKind::Tool(_) => "tool",
            ContributionKind::Service(_) => "service",
            ContributionKind::Keymap(_) => "keymap",
            ContributionKind::Theme(_) => "theme",
            ContributionKind::ProjectionStage(_) => "stage",
            ContributionKind::UiMount(_) => "ui",
            ContributionKind::Guard(_) => "guard",
            ContributionKind::Settings(_) => "settings",
            ContributionKind::Hook(_) => "hook",
        }
    }

    /// The unique identity this contribution occupies within its kind, or
    /// `None` for the layered/overlay kinds (`keymap`, `theme`, `settings`),
    /// for `guard` (monotonicity forbids implicit replacement), and for `hook`
    /// (multiple modules may hook the same kind — hooks always merge).
    ///
    /// Decision 28 precedence-driven implicit replacement: a higher-precedence
    /// layer replaces the holder of the same `(scope, identity)` key; layers
    /// whose kinds return `None` always merge instead. The key is transient
    /// (never serialized), so the composition digest domain is unchanged.
    pub fn override_identity(&self) -> Option<String> {
        let identity = match self {
            ContributionKind::Command(c) => format!("command\u{1f}{}", c.name),
            ContributionKind::Tool(t) => format!("tool\u{1f}{}", t.name),
            ContributionKind::Service(s) => format!("service\u{1f}{}", s.key.name),
            ContributionKind::ProjectionStage(p) => {
                format!("stage\u{1f}{}\u{1f}{}", p.slot, p.ordering)
            }
            ContributionKind::UiMount(u) => format!("ui\u{1f}{}", u.name),
            ContributionKind::Keymap(_)
            | ContributionKind::Theme(_)
            | ContributionKind::Guard(_)
            | ContributionKind::Settings(_)
            | ContributionKind::Hook(_) => return None,
        };
        Some(identity)
    }
}

/// A command: unique per (scope, name) or explicitly replaced (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CommandContribution {
    pub name: String,
    /// Entry name (handler) of the command.
    pub handler: String,
}

/// A tool: unique per (scope, name) or explicitly replaced (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ToolContribution {
    pub name: String,
    /// Kernel-validated tool manifest. The R-04 replay-relevance declaration
    /// lives here: `{ "replay_relevant": bool, ... }`.
    pub manifest: serde_json::Value,
    pub handler: String,
}

/// A service publication: one provider per scoped key (R-25/C-06).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceContribution {
    pub key: ServiceKey,
    pub provider: ServiceProvider,
    pub deps: Vec<ServiceDependency>,
}

/// The UI context a lookup runs under (decision 29): whether a modal focus
/// boundary is active and whether a non-modal `layer` node is present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyContext {
    pub modal: bool,
    pub overlay: bool,
}

/// The context a keybinding is live under (decision 29). `Modal` matches only
/// while a modal focus boundary is active; `Overlay` only while a non-modal
/// `layer` node is present in the tree; `Always` matches in every context.
/// Overlay and Modal outrank every origin tier.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextPredicate {
    #[default]
    Always,
    Modal,
    Overlay,
}

impl ContextPredicate {
    /// Whether this predicate matches `ctx`.
    pub fn matches(self, ctx: KeyContext) -> bool {
        match self {
            ContextPredicate::Always => true,
            ContextPredicate::Modal => ctx.modal,
            ContextPredicate::Overlay => ctx.overlay,
        }
    }

    /// Dispatch tier within the context dimension: `Always` is the lowest,
    /// `Overlay` above it, `Modal` highest (decision 29).
    pub fn rank(self) -> u8 {
        match self {
            ContextPredicate::Always => 0,
            ContextPredicate::Overlay => 1,
            ContextPredicate::Modal => 2,
        }
    }
}

/// Kernel-assigned dispatch origin of a binding (decision 29), derived from
/// the publishing module's origin: `Builtin < Plugin < UserConfig`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeymapOrigin {
    #[default]
    Builtin,
    Plugin,
    UserConfig,
}

impl KeymapOrigin {
    /// Dispatch tier within the origin dimension (decision 29).
    pub fn rank(self) -> u8 {
        match self {
            KeymapOrigin::Builtin => 0,
            KeymapOrigin::Plugin => 1,
            KeymapOrigin::UserConfig => 2,
        }
    }
}

/// A keybinding: a key, the context predicate it is live under, and the
/// action (command) id the kernel routes to the focused mount. Layered match:
/// duplicates merge as layers, and lookup returns the highest matching layer
/// for the current context (R-19, decision 29).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Keybinding {
    pub key: String,
    #[serde(default)]
    pub context: ContextPredicate,
    pub action: String,
    /// Kernel-stamped from the publishing generation's origin; never
    /// module-supplied. Serialized so a registry snapshot round-trip
    /// preserves the dispatch tier (a skipped field would deserialize to the
    /// `Builtin` default, a silent precedence downgrade).
    #[serde(default)]
    pub origin: KeymapOrigin,
    /// Kernel-stamped identity of the publishing module (never
    /// module-supplied), so a binding can be attributed to its OWNER: a
    /// degraded mount disables only bindings published by its own module, not
    /// every binding sharing the scope. `None` when attribution is unknown
    /// (e.g. a replayed snapshot): an unattributable binding is never treated
    /// as degraded (fail-safe forward, never silently swallowed).
    #[serde(default)]
    pub owner: Option<Id128>,
}

/// A theme overlay: validated overlay — later overlays merge over earlier
/// ones (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ThemeContribution {
    pub name: String,
    pub overlay: serde_json::Value,
}

/// A projection-stage slot with an explicit ordering (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProjectionStageContribution {
    pub slot: String,
    pub ordering: u32,
    pub handler: String,
}

/// A named UI mount point (R-19). `slot` names the composite region the
/// mount renders into (M8 multi-module composition): the canonical slots are
/// `"main"` (the default when `None`), `"status"`, `"header"`, `"composer"`,
/// and `"aux"`; any free-form string matching the kernel charset
/// (alphanumeric + `-` + `_`, max 32 chars) is accepted. The kernel orders
/// mounts deterministically by (slot, scope path, name) and fans input out to
/// every mount's reducer (the event carries the focused mount's slot as a
/// `target` hint; each reducer decides).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UiMountContribution {
    pub name: String,
    pub component: String,
    #[serde(default)]
    pub slot: Option<String>,
}

/// A guard: monotonic (R-19) — a monotonic guard cannot be removed or
/// replaced by a non-monotonic one. M2 checks only the monotonic bit; exact
/// predicate-superset analysis is deferred (documented in registry.rs).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GuardContribution {
    pub name: String,
    /// Entry name of the guard predicate.
    pub predicate: String,
    pub monotonic: bool,
}

/// A settings overlay: at most one effective entry per scope; later layers
/// merge field-wise over earlier ones, so a partial layer never clobbers
/// fields it does not set (R-19 layered/overlay semantics). `Default` is the
/// empty overlay (every field unset) — the settings a session reports when no
/// layer contributed any.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct SettingsContribution {
    pub provider: Option<ProviderSettings>,
    pub approval: Option<ApprovalSettings>,
}

/// Provider-side settings (R-19): every field is optional so a layer can
/// override just the parts it owns.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ProviderSettings {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub protocol: Option<String>,
    pub key: Option<KeyReference>,
    pub fake: Option<bool>,
}

/// A reference to a secret, never the secret itself: resolved at use time
/// from the environment or the platform keychain.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum KeyReference {
    Env { name: String },
    Keychain { service: String, account: String },
}

/// Approval-policy settings (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ApprovalSettings {
    pub auto_approve: Option<bool>,
    pub yolo: Option<bool>,
}

/// The kernel-initiated lifecycle seams a module may hook (T9). Hooks are
/// multiplexed over the guest's single cached entry point (`kb_hot`); see
/// `kanbei-modules`'s activation shim and hook multiplexer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    OnTurnStart,
    OnToolIntent,
}

impl HookKind {
    /// The stable wire name used on the guest/kernel envelope.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookKind::OnTurnStart => "on_turn_start",
            HookKind::OnToolIntent => "on_tool_intent",
        }
    }
}

/// A hook: a merge-only contribution (like `keymap`/`guard`) — multiple
/// modules may hook the same [`HookKind`], so precedence never replaces a
/// hook. `name` is a non-canonical identifier within `(scope, hook)`; dispatch
/// is by hook kind over the guest's `kb_hot` multiplexer, so no entry name is
/// carried (T9/H).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookContribution {
    pub name: String,
    pub hook: HookKind,
}
