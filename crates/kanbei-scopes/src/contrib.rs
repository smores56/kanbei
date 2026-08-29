//! Typed structural contributions (R-19/A-11/C): modules contribute typed
//! entries, never resolution logic; the kernel owns the fixed per-type
//! conflict rules.

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
    Keymap(KeymapContribution),
    Theme(ThemeContribution),
    ProjectionStage(ProjectionStageContribution),
    UiMount(UiMountContribution),
    Guard(GuardContribution),
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
        }
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

/// A keymap entry: layered match — duplicates merge as layers, and lookup
/// returns the last matching layer (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct KeymapContribution {
    pub key: String,
    pub action: String,
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

/// A named UI mount point (R-19).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UiMountContribution {
    pub name: String,
    pub component: String,
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
