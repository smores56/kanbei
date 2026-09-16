//! XDG base directories and the kanbei state layout (decision 33/T14).
//!
//! This lives in `kanbei-core`, the lowest crate both `kanbei-session` and
//! `kanbei-memory` already depend on, and the layout is expressed in terms of
//! core's branded [`Id128`] session ids and [`Digest`] content identities. A
//! dedicated `kanbei-paths` crate would add a manifest plus workspace wiring
//! without buying any reuse, so the module stays here.
//!
//! Resolution is pure and filesystem-free: every function returns a path
//! derived from explicit inputs, or `None`. The process environment is read
//! only by the `from_env` convenience wrapper; the testable forms take the
//! variables as arguments, so tests never mutate the environment.
//!
//! Per the XDG base-directory spec — and mirroring
//! `kanbei_session::discovery` — a base variable that is empty or relative
//! counts as unset and the `$HOME` fallback is used. When `$HOME` is likewise
//! missing/empty the root is undetermined and `None` is returned: callers
//! degrade (config discovery skips the layer) rather than panicking or
//! silently falling back to a cwd-relative path.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::id::Id128;

/// The application directory name appended to every XDG base.
const APP: &str = "kanbei";

/// `$XDG_STATE_HOME/kanbei`, falling back to `$HOME/.local/state/kanbei`.
/// `None` when neither variable yields a usable root.
pub fn state_root(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    base_root(xdg_state_home, Path::new(".local/state"), home)
}

/// `$XDG_CONFIG_HOME/kanbei`, falling back to `$HOME/.config/kanbei`.
/// `None` when neither variable yields a usable root.
pub fn config_root(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    base_root(xdg_config_home, Path::new(".config"), home)
}

/// `$XDG_CACHE_HOME/kanbei`, falling back to `$HOME/.cache/kanbei`.
/// `None` when neither variable yields a usable root.
pub fn cache_root(xdg_cache_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    base_root(xdg_cache_home, Path::new(".cache"), home)
}

/// Shared XDG resolution: an absolute, non-empty base variable wins; otherwise
/// `$HOME/<home_rel>`. Empty and relative values count as unset (the spec only
/// defines absolute bases), so they fall through to `$HOME`.
fn base_root(xdg: Option<&OsStr>, home_rel: &Path, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(xdg) = xdg.filter(|v| !v.is_empty()) {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return Some(xdg.join(APP));
        }
    }
    let home = home.filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(home_rel).join(APP))
}

/// The kanbei state layout (decision 33): pure path derivations under one state
/// root. Holds no handles and performs no filesystem I/O — `Session::open`
/// derives the session dir/log/manifest, memory root and projection from it
/// when a layout is configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateLayout {
    root: PathBuf,
}

impl StateLayout {
    /// A layout over an explicit root — the test/override seam.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the layout from the process environment (`$XDG_STATE_HOME`,
    /// `$HOME`); `None` when neither yields a root.
    pub fn from_env() -> Option<Self> {
        Self::discover(
            env::var_os("XDG_STATE_HOME").as_deref(),
            env::var_os("HOME").as_deref(),
        )
    }

    /// Env-explicit form of [`StateLayout::from_env`] (unit-testable without
    /// mutating the process environment).
    pub fn discover(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<Self> {
        state_root(xdg_state_home, home).map(Self::new)
    }

    /// The state root itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<state>/sessions/<SessionId>` — the directory name is the session id's
    /// canonical base58 text.
    pub fn session_dir(&self, session: Id128) -> PathBuf {
        self.root.join("sessions").join(session.to_string())
    }

    /// `<state>/sessions/<SessionId>/events.jsonl.zst` — the canonical log.
    pub fn session_log(&self, session: Id128) -> PathBuf {
        self.session_dir(session).join("events.jsonl.zst")
    }

    /// `<state>/sessions/<SessionId>/session.json` — the persisted manifest.
    pub fn session_manifest(&self, session: Id128) -> PathBuf {
        self.session_dir(session).join("session.json")
    }

    /// `<state>/memory` — the memory substrate root (scope dirs live under it).
    pub fn memory_root(&self) -> PathBuf {
        self.root.join("memory")
    }

    /// `<state>/projection.sqlite` — the disposable SQLite projection.
    pub fn projection_path(&self) -> PathBuf {
        self.root.join("projection.sqlite")
    }

    /// `<state>/modules` — the global content-addressed module store
    /// (decision 33/T14). Module packages install flat under it as
    /// `modules/<digest alg:hex>`, one file per digest — the same on-disk
    /// form as the session object store — so a package digest is shared by
    /// every session under the state root and survives session deletion;
    /// `docs/architecture.md`'s `modules/<package-digest>` names that file.
    pub fn module_root(&self) -> PathBuf {
        self.root.join("modules")
    }

    /// `<state>/memory/projects/events.jsonl.zst` — the project locator stream.
    pub fn projects_log(&self) -> PathBuf {
        self.memory_root().join("projects").join("events.jsonl.zst")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[test]
    fn state_root_prefers_absolute_xdg() {
        assert_eq!(
            state_root(Some(abs("/xdg/state")), Some(abs("/home/u"))),
            Some(PathBuf::from("/xdg/state/kanbei"))
        );
    }

    #[test]
    fn state_root_rejects_relative_xdg() {
        assert_eq!(
            state_root(Some(abs("relative/state")), Some(abs("/home/u"))),
            Some(PathBuf::from("/home/u/.local/state/kanbei"))
        );
    }

    #[test]
    fn state_root_rejects_empty_xdg() {
        assert_eq!(
            state_root(Some(abs("")), Some(abs("/home/u"))),
            Some(PathBuf::from("/home/u/.local/state/kanbei"))
        );
    }

    #[test]
    fn state_root_falls_back_to_home() {
        assert_eq!(
            state_root(None, Some(abs("/home/u"))),
            Some(PathBuf::from("/home/u/.local/state/kanbei"))
        );
    }

    #[test]
    fn state_root_degrades_without_home() {
        assert_eq!(state_root(None, None), None);
        assert_eq!(state_root(Some(abs("rel")), Some(abs(""))), None);
    }

    #[test]
    fn config_root_uses_xdg_or_home() {
        assert_eq!(
            config_root(Some(abs("/xdg/cfg")), None),
            Some(PathBuf::from("/xdg/cfg/kanbei"))
        );
        assert_eq!(
            config_root(None, Some(abs("/home/u"))),
            Some(PathBuf::from("/home/u/.config/kanbei"))
        );
        assert_eq!(config_root(None, None), None);
    }

    #[test]
    fn cache_root_uses_xdg_or_home() {
        assert_eq!(
            cache_root(Some(abs("/xdg/cache")), None),
            Some(PathBuf::from("/xdg/cache/kanbei"))
        );
        assert_eq!(
            cache_root(None, Some(abs("/home/u"))),
            Some(PathBuf::from("/home/u/.cache/kanbei"))
        );
        assert_eq!(cache_root(Some(abs("")), None), None);
    }

    #[test]
    fn discover_reads_explicit_env() {
        assert_eq!(
            StateLayout::discover(Some(abs("/xdg/state")), None)
                .unwrap()
                .root(),
            Path::new("/xdg/state/kanbei")
        );
        assert_eq!(
            StateLayout::discover(None, Some(abs("/home/u")))
                .unwrap()
                .root(),
            Path::new("/home/u/.local/state/kanbei")
        );
        assert_eq!(StateLayout::discover(Some(abs("rel")), None), None);
    }

    #[test]
    fn layout_override_ignores_env() {
        let layout = StateLayout::new("/tmp/test-state");
        assert_eq!(layout.root(), Path::new("/tmp/test-state"));
    }

    #[test]
    fn layout_derivations() {
        let layout = StateLayout::new("/s");
        let session = Id128::from_bytes([1; 16]);
        assert_eq!(
            layout.session_dir(session),
            PathBuf::from(format!("/s/sessions/{session}"))
        );
        assert_eq!(
            layout.session_log(session),
            PathBuf::from(format!("/s/sessions/{session}/events.jsonl.zst"))
        );
        assert_eq!(
            layout.session_manifest(session),
            PathBuf::from(format!("/s/sessions/{session}/session.json"))
        );
        assert_eq!(layout.memory_root(), PathBuf::from("/s/memory"));
        assert_eq!(
            layout.projection_path(),
            PathBuf::from("/s/projection.sqlite")
        );
        assert_eq!(
            layout.projects_log(),
            PathBuf::from("/s/memory/projects/events.jsonl.zst")
        );
    }

    #[test]
    fn layout_module_root_holds_the_shared_store() {
        assert_eq!(
            StateLayout::new("/s").module_root(),
            PathBuf::from("/s/modules")
        );
    }
}
