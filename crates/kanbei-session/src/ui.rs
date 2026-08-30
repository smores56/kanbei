//! M5 semantic workbench: the session-owned UI host. The kernel boundary
//! (kanbei-ui) owns input decoding, focus, rendering, diffing, and fallback;
//! this host wires it to the module substrate and the session spine:
//!
//! - the built-in UI is an immutable module generation activated through the
//!   standard contribution contract (UI mount + theme overlay staged via
//!   `contribution_publish` and atomically OCC-published);
//! - M8 multi-module composition: EVERY root-scope UI mount binds, ordered
//!   deterministically by (slot, scope path, name); the host composes the
//!   mount trees into one synthetic root (each mount's root stays a child,
//!   ids prefixed so focus identity is unambiguous) and the existing frame
//!   render pipeline renders the composite unchanged. Input events fan out to
//!   every mount's reducer in slot order, carrying the focused mount's slot
//!   as a `target` hint; each reducer decides. Intents are capability-checked
//!   per mount with THAT mount's generation grants (capability isolation);
//!   a mount fault degrades only that mount (placeholder subtree), the
//!   others keep working;
//! - module-emitted intents are capability-checked (R-27: subject to the
//!   standard capability intersection) and produce canonical domain facts
//!   (e.g. `user_message`), never gestures;
//! - fault classes (R-27): composition failure → staleness banner on the
//!   last-valid UI; runtime component fault → kernel placeholder + degraded
//!   (per mount since M8); kernel render fault → kernel fallback UI (safe
//!   mode).
//!
//! Composition rule (M8): the composite root is a synthetic, never-focusable
//! `Root` node whose children are the mount roots in slot order; the kernel
//! status bar and the focused input line stay kernel-owned at the bottom,
//! exactly as in the single-mount workbench.

use std::io;

use kanbei_capabilities::{Capability, Principal};
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_modules::ModuleManager;
use kanbei_ui::accessibility;
use kanbei_ui::fallback;
use kanbei_ui::focus::{FocusDirection, InputClass, KeyClassifier, ReservedAction};
use kanbei_ui::frame::{RenderContext, render};
use kanbei_ui::input::{InputDecoder, InputEvent, UiEvent, UiEventKind};
use kanbei_ui::theme::Theme;
use kanbei_ui::tree::{NodeKind, SemanticTree};
use kanbei_ui::diff::{FrameDiff, apply, paint_full};
use kanbei_ui::{Terminal, TerminalFrame};
use serde_json::{Value, json};

use crate::{FaultPoint, NewEvent, Session, SessionError};

/// The resource UI intents check against the broker (R-27 capability
/// intersection). Verbs: `append` (submit text), `cancel` (cancel the active
/// run).
pub const UI_INTENT_RESOURCE: &str = "session";

/// A normalized intent emitted by the UI module (R-27: persist intents/facts,
/// never gestures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiIntent {
    SubmitText { text: String },
    CancelRun,
}

impl UiIntent {
    fn from_json(v: &Value) -> Option<UiIntent> {
        match v.get("kind").and_then(Value::as_str) {
            Some("submit_text") => Some(UiIntent::SubmitText {
                text: v
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            Some("cancel_run") => Some(UiIntent::CancelRun),
            _ => None,
        }
    }
}

/// One bound UI mount: a root-scope `UiMountContribution` resolved to the
/// generation that mounted its component (M8 multi-module composition).
/// Mounts keep their own opaque reducer state, last validated tree, and
/// fault/intent accounting; the kernel composes their trees into one
/// synthetic root (`SemanticTree::compose`) and fans input out to every
/// mount's reducer (each reducer decides; the event carries the focused
/// mount's slot as a `target` hint).
#[derive(Debug)]
pub struct BoundMount {
    /// The composite region this mount renders into (canonical slots:
    /// `main`, `status`, `header`, `composer`, `aux`; `None` is the default
    /// `main` and is normalized by the registry at publish).
    pub slot: String,
    /// The mount's contribution name (unique per root scope).
    pub name: String,
    /// The UI component entry (the generation's `kb_hot` ui entry).
    pub component: String,
    pub generation: u64,
    /// The mount's last validated tree with ORIGINAL ids (the composite view
    /// prefixes them). `None` until the first render.
    pub tree: Option<SemanticTree>,
    /// Last focused node id within this mount (original ids); restored when
    /// Tab cycles back into the mount.
    pub focus: Option<String>,
    /// Opaque reducer state returned by the mount's `ui_reduce`.
    pub reducer_state: Value,
    /// Runtime component fault flag (R-27 fault class 2): the kernel renders
    /// a placeholder for this mount until a successful reduce/render clears
    /// it. Other mounts keep working (M8 fault isolation).
    pub degraded: bool,
    pub last_error: Option<String>,
    /// Intents dropped by the capability intersection (per-mount grants).
    pub denied_intents: u64,
    /// Intents the last reduce returned, awaiting capability intersection.
    pending_intents: Vec<UiIntent>,
}

impl BoundMount {
    fn new(slot: String, name: String, component: String, generation: u64) -> Self {
        BoundMount {
            slot,
            name,
            component,
            generation,
            tree: None,
            focus: None,
            reducer_state: Value::Null,
            degraded: false,
            last_error: None,
            denied_intents: 0,
            pending_intents: Vec::new(),
        }
    }
}

/// The session-side UI host: the bound mounts in deterministic slot order,
/// the kernel-owned interaction state (focus, decoder, classifier, theme,
/// last frame) over the COMPOSITE tree, and the composition-level fault
/// flags (staleness, safe mode). `component`/`generation`/`degraded`/
/// `last_error`/`denied_intents` are summary mirrors of the bound mounts
/// (the primary = first mount) kept for the M5 single-mount API.
pub struct UiHost {
    /// Bound mounts in deterministic (slot, scope path, name) order — the
    /// composite child order.
    pub mounts: Vec<BoundMount>,
    /// Primary mount's component (M5 single-mount API mirror).
    pub component: String,
    /// Primary mount's generation (M5 single-mount API mirror).
    pub generation: u64,
    /// Any mount degraded (M5 single-mount API mirror).
    pub degraded: bool,
    /// First mount's last error (M5 single-mount API mirror).
    pub last_error: Option<String>,
    /// Composition staleness banner (R-27 fault class 1).
    pub staleness: Option<String>,
    /// Kernel safe mode: fallback UI, module input dropped (R-27).
    pub safe_mode: bool,
    /// Intents dropped by the capability intersection across all mounts.
    pub denied_intents: u64,
    /// Kernel focus over the composite tree (prefixed ids; unambiguous
    /// across mounts).
    pub focus: kanbei_ui::FocusModel,
    classifier: KeyClassifier,
    decoder: InputDecoder,
    theme: Theme,
    /// The composed synthetic tree (or the kernel fallback tree in safe
    /// mode).
    last_tree: Option<SemanticTree>,
    last_frame: Option<TerminalFrame>,
    last_diff: FrameDiff,
    size: (u16, u16),
    pub viewport_top: usize,
    pub last_status: String,
}

/// What one `ui_handle_input` pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiOutcome {
    pub intents_applied: usize,
    pub denied: u64,
    pub degraded: bool,
    pub staleness: Option<String>,
    pub safe_mode: bool,
    pub repaint: bool,
}

impl UiHost {
    fn bind(theme: Theme, mounts: Vec<BoundMount>) -> Self {
        let mut host = UiHost {
            mounts,
            component: String::new(),
            generation: 0,
            degraded: false,
            last_error: None,
            staleness: None,
            safe_mode: false,
            denied_intents: 0,
            focus: kanbei_ui::FocusModel::new(),
            classifier: KeyClassifier::new(),
            decoder: InputDecoder::new(),
            theme,
            last_tree: None,
            last_frame: None,
            last_diff: FrameDiff::default(),
            size: (24, 80),
            viewport_top: 0,
            last_status: "idle".to_string(),
        };
        host.sync_summary();
        host
    }

    /// Recompute the summary mirrors from the bound mounts.
    fn sync_summary(&mut self) {
        let primary = self.mounts.first();
        self.component = primary.map(|m| m.component.clone()).unwrap_or_default();
        self.generation = primary.map(|m| m.generation).unwrap_or(0);
        self.degraded = self.mounts.iter().any(|m| m.degraded);
        self.last_error = self.mounts.iter().find_map(|m| m.last_error.clone());
        self.denied_intents = self.mounts.iter().map(|m| m.denied_intents).sum();
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn last_tree(&self) -> Option<&SemanticTree> {
        self.last_tree.as_ref()
    }

    pub fn last_frame(&self) -> Option<&TerminalFrame> {
        self.last_frame.as_ref()
    }

    pub fn last_diff(&self) -> &FrameDiff {
        &self.last_diff
    }
}

impl Session {
    /// Activate a UI module generation through the standard contribution
    /// contract: atomic config-activation path (validate → OCC publish →
    /// canonical `composition_changed`), then bind the UI host to ALL
    /// root-scope UI mounts of the composition, ordered deterministically by
    /// (slot, scope path, name). Any failure retains the last-valid
    /// composition (R-01/C-02) and marks the UI stale. Returns the
    /// composition epoch.
    pub fn activate_ui(&mut self, manifest: PackageManifest) -> Result<u64, SessionError> {
        let activation = self.activate_config(manifest)?;
        self.rebind_ui(activation.generation)?;
        Ok(self.composition().epoch)
    }

    /// Activate the built-in workbench UI (an immutable module generation,
    /// kernel-trusted). The kernel grants the builtin's generation the
    /// session intents its UI emits (`append`/`cancel`); custom UI modules
    /// carry no grants, so their intents are subject to (and denied by) the
    /// standard capability intersection until the user grants them.
    pub fn activate_builtin_ui(&mut self) -> Result<u64, SessionError> {
        let manifest = PackageManifest {
            schema: 1,
            module_id: Id128::generate(),
            origin: ModuleOrigin::Builtin,
            trust_class: kanbei_capabilities::TrustClass::Builtin,
            scope: kanbei_services::ScopePath(vec!["root".into()]),
            deps: Vec::new(),
            capabilities: Vec::new(),
            source: kanbei_ui::BUILTIN_UI_SOURCE.to_string(),
            state_schema: None,
        };
        let activation = self.activate_config(manifest)?;
        self.rebind_ui(activation.generation)?;
        let generation = activation.generation;
        let epoch = activation.epoch;
        // Kernel default policy for the builtin UI: when no Builtin-class
        // template is configured, the kernel installs its default (the
        // builtin may submit text / cancel runs). An explicitly configured
        // template always wins (R-13 default-deny stands otherwise).
        if !self
            .broker
            .templates
            .iter()
            .any(|t| t.trust_class == kanbei_capabilities::TrustClass::Builtin)
        {
            self.broker
                .add_template(kanbei_capabilities::PolicyTemplate {
                    trust_class: kanbei_capabilities::TrustClass::Builtin,
                    allow: vec![
                        Capability::new("session".into(), vec!["append".into()]),
                        Capability::new("session".into(), vec!["cancel".into()]),
                    ],
                    deny: vec![],
                    require_approval: vec![],
                    version: 1,
                    monotonic: true,
                })
                .map_err(|e| SessionError::InvalidInput(format!("ui policy: {e}")))?;
        }
        let policy_version = self.broker.policy_version();
        for verb in ["append", "cancel"] {
            let mut grant = kanbei_capabilities::Grant {
                grant_digest: kanbei_core::digest::Digest::new(b"builtin-ui"),
                principal: Principal {
                    session: self.session_id(),
                    generation,
                    run: None,
                },
                module_generation: generation,
                capability: Capability::new("session".into(), vec![verb.into()]),
                scope: kanbei_capabilities::GrantScope::Session,
                expiry: None,
                budget: None,
                purpose: Some("builtin workbench UI".into()),
                policy_version,
            };
            grant.grant_digest = grant.derive_digest();
            self.broker
                .add_grant(grant)
                .map_err(|e| SessionError::InvalidInput(format!("ui grant: {e}")))?;
        }
        Ok(epoch)
    }

    /// Bind (or rebind) the UI host to EVERY root-scope UI mount of the
    /// composition and the generation that mounted each component (M8).
    /// Mounts are ordered deterministically by (slot, scope path, name);
    /// mounts whose component no longer resolves to a live generation are
    /// skipped, so a replaced/deactivated generation's mounts unbind and the
    /// remaining ones rebind in order. The theme merges every bound mount's
    /// overlay in bind order. An empty result unbinds the host.
    pub(crate) fn rebind_ui(&mut self, _generation: u64) -> Result<(), SessionError> {
        let Some(manager) = self.modules.as_ref() else {
            return Ok(());
        };
        let root = kanbei_services::ScopePath(vec!["root".into()]);
        let mut mounts: Vec<(String, String, String)> = self
            .registry
            .snapshot()
            .into_iter()
            .filter_map(|c| match c.kind {
                kanbei_scopes::contrib::ContributionKind::UiMount(m) if c.scope == root => Some((
                    m.slot.unwrap_or_else(|| "main".to_string()),
                    m.name,
                    m.component,
                )),
                _ => None,
            })
            .collect();
        mounts.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let mut bound: Vec<BoundMount> = Vec::new();
        let mut theme = Theme::default_theme();
        for (slot, name, component) in mounts {
            let Some(generation) = manager.ui_generation(&component) else {
                continue;
            };
            if let Some(overlay) = self.registry.theme_overlay(&root, &name) {
                let _ = theme.apply_overlay(&overlay.overlay);
            }
            bound.push(BoundMount::new(slot, name, component, generation));
        }
        self.ui_host = if bound.is_empty() {
            None
        } else {
            Some(UiHost::bind(theme, bound))
        };
        Ok(())
    }

    /// The bound UI host, if any.
    pub fn ui(&self) -> Option<&UiHost> {
        self.ui_host.as_ref()
    }

    pub fn ui_mut(&mut self) -> Option<&mut UiHost> {
        self.ui_host.as_mut()
    }

    /// The kernel status text for the status bar.
    pub fn ui_status_text(&self) -> String {
        if self.ui_host.as_ref().is_none_or(|u| u.safe_mode) {
            return "safe mode".to_string();
        }
        if self.scheduler.is_paused() {
            return "paused".to_string();
        }
        match self.scheduler.active_run() {
            Some(_) => "running".to_string(),
            None => "idle".to_string(),
        }
    }

    /// Append a user message: canonical `user_message` fact + responder
    /// trigger (the UI's SubmitText lands here).
    pub fn append_user_message(&mut self, text: &str) -> Result<u64, SessionError> {
        let receipt = self.commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({ "text": text }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.scheduler.observe(kanbei_scheduler::Trigger {
            kind: kanbei_scheduler::TriggerKind::UserMessage,
            referent: None,
        });
        Ok(receipt.last_seq)
    }

    /// Feed raw terminal bytes through the kernel boundary: decode +
    /// sanitize, reserved-key handling (cancel/repaint/safe-mode chord),
    /// focus navigation, per-mount module reduce (fan-out), per-mount
    /// intents, summary sync. Returns the outcome; the frame is available
    /// via `ui().last_frame()` and presented by [`Session::ui_present`].
    pub fn ui_handle_input(&mut self, bytes: &[u8]) -> Result<UiOutcome, SessionError> {
        let mut outcome = UiOutcome::default();
        // Decode + sanitize (kernel boundary); host borrows are per-step so
        // the session's own methods can run in between.
        let events = match self.ui_host.as_mut() {
            Some(host) => {
                if host.safe_mode {
                    // Fallback UI: input is not forwarded to modules (R-27).
                    let _ = host.decoder.feed(bytes);
                    outcome.repaint = true;
                    return Ok(outcome);
                }
                let mut events = host.decoder.feed(bytes);
                events.extend(host.decoder.finish());
                events
            }
            None => return Ok(outcome),
        };
        for event in events {
            let class = self
                .ui_host
                .as_mut()
                .map(|host| host.classifier.classify(&event))
                .unwrap_or(InputClass::Forward);
            match class {
                InputClass::Reserved(ReservedAction::CancelRun) => {
                    let _ = self.cancel_active_run()?;
                    outcome.repaint = true;
                }
                InputClass::Reserved(ReservedAction::Repaint) => outcome.repaint = true,
                InputClass::Reserved(ReservedAction::SafeModeChord) => {
                    self.enter_ui_safe_mode()?;
                    outcome.safe_mode = true;
                    outcome.repaint = true;
                }
                InputClass::Consumed => {}
                InputClass::Forward => self.ui_forward(&event, &mut outcome)?,
            }
        }
        if let Some(host) = self.ui_host.as_mut() {
            host.sync_summary();
            outcome.degraded = host.degraded;
            outcome.staleness = host.staleness.clone();
            outcome.denied = host.denied_intents;
        }
        Ok(outcome)
    }

    /// Enter kernel safe mode from the reserved chord: canonical fact +
    /// fallback UI (module input dropped).
    fn enter_ui_safe_mode(&mut self) -> Result<(), SessionError> {
        if let Some(host) = self.ui_host.as_mut() {
            host.safe_mode = true;
            host.last_tree = Some(fallback::FallbackUi::new("safe mode").tree());
        }
        self.commit(
            vec![NewEvent {
                kind: "safe_mode_activated".into(),
                payload_schema: 1,
                payload: json!({ "reason": "ui safe-mode chord" }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        Ok(())
    }

    /// Forward one non-reserved event: navigation stays kernel-side; text
    /// and activation go to every mount's reducer.
    fn ui_forward(&mut self, event: &InputEvent, outcome: &mut UiOutcome) -> Result<(), SessionError> {
        match event {
            InputEvent::Tab => {
                self.ui_focus_move(FocusDirection::Next);
                outcome.repaint = true;
            }
            InputEvent::ShiftTab => {
                self.ui_focus_move(FocusDirection::Prev);
                outcome.repaint = true;
            }
            InputEvent::ArrowUp => {
                self.ui_focus_move(FocusDirection::Up);
                outcome.repaint = true;
            }
            InputEvent::ArrowDown => {
                self.ui_focus_move(FocusDirection::Down);
                outcome.repaint = true;
            }
            InputEvent::ArrowLeft => {
                self.ui_focus_move(FocusDirection::Left);
                outcome.repaint = true;
            }
            InputEvent::ArrowRight => {
                self.ui_focus_move(FocusDirection::Right);
                outcome.repaint = true;
            }
            _ => {
                let focused = self.ui_host.as_ref().and_then(|u| u.focus.focused.clone());
                let tree = self.ui_host.as_ref().and_then(|u| u.last_tree.clone());
                let kind = match event.to_ui(focused.as_deref()) {
                    Some(k) => k,
                    None => return Ok(()),
                };
                // Enter on a focused button resolves to an activation event
                // (the composite tree carries the focused node; the composite
                // id is split back to the mount's original id at reduce).
                let kind = match (&kind, focused.as_deref()) {
                    (UiEventKind::Enter, Some(id)) => match tree
                        .as_ref()
                        .and_then(|t| t.node(id))
                    {
                        Some(n) if n.kind == NodeKind::Button => UiEventKind::Activate(id.to_string()),
                        _ => kind,
                    },
                    _ => kind,
                };
                self.ui_reduce(UiEvent::user(kind))?;
                let applied = self.apply_ui_intents()?;
                outcome.intents_applied += applied;
            }
        }
        Ok(())
    }

    /// Kernel focus navigation (M8): Tab/Shift-Tab cycle the next/prev
    /// focusable node across ALL mount subtrees in slot order (the composite
    /// ring); Up/Down stay within the focused mount's subtree. Cross-mount
    /// moves restore the entered mount's last focused node.
    fn ui_focus_move(&mut self, dir: FocusDirection) {
        let Some(host) = self.ui_host.as_mut() else {
            return;
        };
        let Some(tree) = host.last_tree.clone() else {
            return;
        };
        let prev = host.focus.focused.clone();
        // Remember where the user was, per mount (original ids).
        if let Some((i, original)) = prev.as_deref().and_then(SemanticTree::split_composite_id)
            && let Some(mount) = host.mounts.get_mut(i)
        {
            mount.focus = Some(original.to_string());
        }
        match dir {
            FocusDirection::Up | FocusDirection::Down => {
                // Arrows stay within the focused mount's subtree.
                let boundary: Option<String> = prev
                    .as_deref()
                    .and_then(SemanticTree::split_composite_id)
                    .and_then(|(i, _)| {
                        host.mounts
                            .get(i)
                            .and_then(|m| m.tree.as_ref())
                            .map(|t| format!("{i}.{}", t.root.id))
                    });
                match boundary {
                    Some(b) => host.focus.move_focus_within(&tree, dir, &b),
                    None => host.focus.move_focus(&tree, dir),
                }
            }
            _ => {
                host.focus.move_focus(&tree, dir);
                // A Tab crossing into another mount restores its remembered
                // focus instead of landing on its first focusable.
                let entered = host
                    .focus
                    .focused
                    .as_deref()
                    .and_then(SemanticTree::split_composite_id);
                let left = prev.as_deref().and_then(SemanticTree::split_composite_id);
                if let (Some((i, _)), Some((j, _))) = (entered, left)
                    && i != j
                {
                    let remembered = host.mounts[i].focus.clone();
                    if let Some(original) = remembered {
                        let candidate = format!("{i}.{original}");
                        if tree.is_focusable(&candidate) {
                            host.focus.focused = Some(candidate);
                            host.focus.caret = 0;
                        }
                    }
                }
            }
        }
    }

    /// Call every mount's reducer with the event (M8 fan-out): the kernel
    /// delivers to all mounts in slot order, each with its own state; the
    /// event carries the focused mount's slot as a `target` hint so a
    /// reducer can ignore non-target events. A fault degrades only that
    /// mount (placeholder subtree); the others keep working.
    fn ui_reduce(&mut self, event: UiEvent) -> Result<(), SessionError> {
        self.fault(FaultPoint::BeforeUiReduce);
        self.ui_reduce_inner(event);
        self.fault(FaultPoint::AfterUiReduce);
        Ok(())
    }

    fn ui_reduce_inner(&mut self, event: UiEvent) {
        let Some(host) = self.ui_host.as_mut() else {
            return;
        };
        let Some(manager) = self.modules.as_ref() else {
            return;
        };
        // The target hint: the focused mount's slot (None when nothing is
        // focused). Single-mount ids are unprefixed (M5 byte-identical
        // trees), so an unresolvable id means the one bound mount.
        let target = host
            .focus
            .focused
            .as_deref()
            .and_then(SemanticTree::split_composite_id)
            .and_then(|(i, _)| host.mounts.get(i))
            .map(|m| m.slot.clone())
            .or_else(|| {
                (host.mounts.len() == 1).then(|| host.mounts[0].slot.clone())
            });
        // Activation ids are composite ids; each mount receives its own
        // original id back.
        let event_kind = match &event.kind {
            UiEventKind::Activate(id) => {
                let original = SemanticTree::split_composite_id(id)
                    .map(|(_, o)| o.to_string())
                    .unwrap_or_else(|| id.clone());
                UiEventKind::Activate(original)
            }
            other => other.clone(),
        };
        let event_value: Value = match &event_kind {
            UiEventKind::Char(c) => json!({ "kind": "char", "text": c.to_string() }),
            UiEventKind::Backspace => json!({ "kind": "backspace" }),
            UiEventKind::Enter => json!({ "kind": "enter" }),
            UiEventKind::Activate(id) => json!({ "kind": "activate", "node": id }),
        };
        let mut event_value = event_value;
        if let Some(target) = target {
            event_value["target"] = json!(target);
        }
        for mount in host.mounts.iter_mut() {
            let payload = json!({
                "entry": "ui_reduce",
                "state": mount.reducer_state,
                "event": event_value,
            });
            let out = match manager.call_generation(mount.generation, &payload.to_string()) {
                Ok(out) => out,
                Err(e) => {
                    mount.degraded = true;
                    mount.last_error = Some(e.to_string());
                    mount.tree =
                        Some(fallback::placeholder_tree(&mount.component, &e.to_string()));
                    continue;
                }
            };
            let v: Value = match serde_json::from_str(&out) {
                Ok(v) => v,
                Err(e) => {
                    mount.degraded = true;
                    mount.last_error = Some(format!("ui_reduce: invalid result JSON: {e}"));
                    mount.tree = Some(fallback::placeholder_tree(
                        &mount.component,
                        mount.last_error.as_deref().unwrap_or("reduce failed"),
                    ));
                    continue;
                }
            };
            mount.reducer_state = v.get("state").cloned().unwrap_or(Value::Null);
            mount.pending_intents = v
                .get("intents")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(UiIntent::from_json).collect())
                .unwrap_or_default();
            mount.degraded = false;
            mount.last_error = None;
        }
    }

    /// Apply the pending intents of every mount through the capability
    /// intersection, per mount with THAT mount's generation grants (M8
    /// capability isolation: a mount without a grant has its intent denied
    /// while another mount's identical intent applies). Accepted intents
    /// apply in slot order. Denied intents are dropped and counted per
    /// mount, never canonical.
    fn apply_ui_intents(&mut self) -> Result<usize, SessionError> {
        let intents: Vec<(usize, u64, Vec<UiIntent>)> = self
            .ui_host
            .as_mut()
            .map(|host| {
                host.mounts
                    .iter_mut()
                    .enumerate()
                    .map(|(i, m)| (i, m.generation, std::mem::take(&mut m.pending_intents)))
                    .collect()
            })
            .unwrap_or_default();
        let mut applied = 0;
        for (mount_index, generation, intents) in intents {
            for intent in intents {
                let principal = Principal {
                    session: self.session_id(),
                    generation,
                    run: None,
                };
                let want = match &intent {
                    UiIntent::SubmitText { .. } => {
                        Capability::new("session".into(), vec!["append".into()])
                    }
                    UiIntent::CancelRun => {
                        Capability::new("session".into(), vec!["cancel".into()])
                    }
                };
                let allowed = self
                    .broker
                    .check(&principal, &want, self.broker.policy_version())
                    .is_ok();
                if !allowed {
                    if let Some(host) = self.ui_host.as_mut()
                        && let Some(mount) = host.mounts.get_mut(mount_index)
                    {
                        mount.denied_intents += 1;
                    }
                    continue;
                }
                match intent {
                    UiIntent::SubmitText { text } => {
                        self.append_user_message(&text)?;
                        applied += 1;
                        let _ = self.ui_refresh("user message committed");
                    }
                    UiIntent::CancelRun => {
                        if self.scheduler.active_run().is_some() {
                            let _ = self.cancel_active_run()?;
                        }
                        applied += 1;
                    }
                }
            }
        }
        Ok(applied)
    }

    /// Re-render the composite of the mount trees into a frame + diff
    /// against the last frame (kernel-owned rendering; the modules produced
    /// only tree data).
    pub fn ui_render_frame(&mut self) -> Result<(), SessionError> {
        let safe_mode = self.ui_host.as_ref().map(|h| h.safe_mode).unwrap_or(true);
        let tree = if safe_mode {
            // Kernel fallback UI (R-27 fault class 3): module input dropped.
            self.ui_host
                .as_mut()
                .and_then(|h| h.last_tree.clone())
                .unwrap_or_else(|| fallback::FallbackUi::new("safe mode").tree())
        } else {
            match self.ui_render_module_tree() {
                Some(tree) => tree,
                None => self
                    .ui_host
                    .as_mut()
                    .and_then(|h| h.last_tree.clone())
                    .unwrap_or_else(|| fallback::placeholder_tree("workbench", "render failed")),
            }
        };
        let size = self.ui_host.as_ref().map(|h| h.size).unwrap_or((24, 80));
        let status = self.ui_status_text();
        let Some(host) = self.ui_host.as_mut() else {
            return Ok(());
        };
        host.last_status = status.clone();
        host.sync_summary();
        let ctx = RenderContext {
            tree: &tree,
            theme: &host.theme,
            focus: &host.focus,
            size,
            status: &status,
            staleness: host.staleness.as_deref(),
            degraded: host.degraded,
        };
        let output = render(&ctx).map_err(|e| SessionError::InvalidInput(e.to_string()))?;
        let diff = match &host.last_frame {
            Some(prev) => kanbei_ui::diff::diff(prev, &output.frame),
            None => FrameDiff::default(),
        };
        host.last_diff = diff;
        host.last_frame = Some(output.frame);
        host.viewport_top = output.viewport_top;
        host.focus.viewport_top = output.viewport_top;
        Ok(())
    }

    /// Render every mount's tree through its generation, kernel-validate
    /// each (accessibility pass is kernel-owned, per mount), and compose the
    /// validated trees into one synthetic root (slot order = child order).
    /// A mount fault contributes its placeholder and degrades only that
    /// mount. Returns None only when there is nothing to render.
    fn ui_render_module_tree(&mut self) -> Option<SemanticTree> {
        self.fault(FaultPoint::BeforeUiRender);
        let result = self.ui_render_module_tree_inner();
        self.fault(FaultPoint::AfterUiRender);
        result
    }

    fn ui_render_module_tree_inner(&mut self) -> Option<SemanticTree> {
        let host = self.ui_host.as_mut()?;
        let manager = self.modules.as_ref()?;
        let mut composed: Vec<(String, SemanticTree)> = Vec::with_capacity(host.mounts.len());
        for mount in host.mounts.iter_mut() {
            let tree = if mount.degraded {
                // Degraded mounts keep their placeholder (no render call).
                mount.tree.clone().unwrap_or_else(|| {
                    fallback::placeholder_tree(&mount.component, "module degraded")
                })
            } else {
                match Self::render_mount(manager, mount) {
                    Some(tree) => tree,
                    None => mount.tree.clone().unwrap_or_else(|| {
                        fallback::placeholder_tree(&mount.component, "render failed")
                    }),
                }
            };
            composed.push((mount.slot.clone(), tree));
        }
        let refs: Vec<(&str, &SemanticTree)> = composed
            .iter()
            .map(|(slot, tree)| (slot.as_str(), tree))
            .collect();
        let composite = SemanticTree::compose(&refs);
        host.last_tree = Some(composite.clone());
        Some(composite)
    }

    /// Render one mount's tree through its generation and kernel-validate it
    /// (accessibility pass is kernel-owned, R-27). On any fault the mount is
    /// degraded with a placeholder and None is returned; other mounts are
    /// untouched (M8 fault isolation).
    fn render_mount(manager: &ModuleManager, mount: &mut BoundMount) -> Option<SemanticTree> {
        let payload = json!({ "entry": "ui_render", "state": mount.reducer_state });
        let out = match manager.call_generation(mount.generation, &payload.to_string()) {
            Ok(out) => out,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(e.to_string());
                mount.tree =
                    Some(fallback::placeholder_tree(&mount.component, &e.to_string()));
                return None;
            }
        };
        let v: Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(format!("ui_render: invalid result JSON: {e}"));
                mount.tree = Some(fallback::placeholder_tree(
                    &mount.component,
                    mount.last_error.as_deref().unwrap_or("render failed"),
                ));
                return None;
            }
        };
        let tree = match SemanticTree::from_json(&v) {
            Ok(tree) => tree,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(format!("ui_render: invalid tree: {e}"));
                mount.tree = Some(fallback::placeholder_tree(
                    &mount.component,
                    mount.last_error.as_deref().unwrap_or("render failed"),
                ));
                return None;
            }
        };
        let errors: Vec<String> = accessibility::validate(&tree)
            .into_iter()
            .filter(|i| i.severity == accessibility::Severity::Error)
            .map(|i| format!("{}: {}", i.node_id, i.message))
            .collect();
        if !errors.is_empty() {
            mount.degraded = true;
            mount.last_error = Some(format!("accessibility: {}", errors.join("; ")));
            mount.tree = Some(fallback::placeholder_tree(
                &mount.component,
                mount.last_error.as_deref().unwrap_or("invalid tree"),
            ));
            return None;
        }
        mount.tree = Some(tree.clone());
        mount.degraded = false;
        mount.last_error = None;
        Some(tree)
    }

    /// Push a kernel facts refresh to every non-degraded mount (e.g. after a
    /// canonical event the UI should reflect).
    pub fn ui_refresh(&mut self, last_outcome: &str) -> Result<(), SessionError> {
        let Some(host) = self.ui_host.as_mut() else {
            return Ok(());
        };
        if host.safe_mode {
            return Ok(());
        }
        let Some(manager) = self.modules.as_ref() else {
            return Ok(());
        };
        for mount in host.mounts.iter_mut() {
            if mount.degraded {
                continue;
            }
            let payload = json!({
                "entry": "ui_reduce",
                "state": mount.reducer_state,
                "event": {"kind": "refresh", "facts": {"last_outcome": last_outcome}},
            });
            match manager.call_generation(mount.generation, &payload.to_string()) {
                Ok(out) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&out) {
                        mount.reducer_state =
                            v.get("state").cloned().unwrap_or(mount.reducer_state.clone());
                    }
                }
                Err(e) => {
                    mount.degraded = true;
                    mount.last_error = Some(e.to_string());
                    mount.tree =
                        Some(fallback::placeholder_tree(&mount.component, &e.to_string()));
                }
            }
        }
        if let Some(host) = self.ui_host.as_mut() {
            host.sync_summary();
        }
        Ok(())
    }

    /// Present the pending frame to a terminal: repaint on size change or
    /// explicit repaint, else write only the diff (kernel render diffing).
    /// A write failure is a kernel render fault: the kernel fallback UI is
    /// rendered instead (R-27 fault class 3).
    pub fn ui_present(&mut self, terminal: &mut dyn Terminal) -> io::Result<()> {
        let Some(host) = self.ui_host.as_mut() else {
            return Ok(());
        };
        let size = terminal.size()?;
        let resized = size != host.size;
        if resized {
            host.size = size;
        }
        let repaint = resized || host.last_frame.is_none();
        self.ui_render_frame()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (frame, diff) = {
            let host = self.ui_host.as_ref().expect("ui host present");
            (host.last_frame.clone().expect("frame rendered"), host.last_diff.clone())
        };
        let theme = self.ui_host.as_ref().expect("ui host present").theme.clone();
        let result = if repaint || diff.is_empty() {
            paint_full(terminal, &frame, &theme)
        } else {
            apply(terminal, &diff, &theme)
        };
        if let Err(e) = result {
            // Kernel render fault: fall back to the kernel fallback UI.
            let host = self.ui_host.as_mut().expect("ui host present");
            host.safe_mode = true;
            host.last_error = Some(format!("kernel render fault: {e}"));
            let fallback_tree = fallback::FallbackUi::new(format!("kernel render fault: {e}")).tree();
            host.last_tree = Some(fallback_tree);
            let size = host.size;
            let ctx = RenderContext {
                tree: host.last_tree.as_ref().expect("fallback tree"),
                theme: &host.theme,
                focus: &host.focus,
                size,
                status: "safe mode",
                staleness: host.staleness.as_deref(),
                degraded: false,
            };
            let output = render(&ctx).map_err(|e| io::Error::other(e.to_string()))?;
            host.last_frame = Some(output.frame);
            host.last_diff = FrameDiff::default();
            paint_full(
                terminal,
                host.last_frame.as_ref().expect("fallback frame"),
                &host.theme,
            )?;
        }
        Ok(())
    }
}
