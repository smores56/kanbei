//! M5 semantic workbench: the session-owned UI host. The kernel boundary
//! (kanbei-ui) owns input decoding, focus, rendering, diffing, and fallback;
//! this host wires it to the module substrate and the session spine:
//!
//! - the built-in UI is an immutable module generation activated through the
//!   standard contribution contract (UI mount + theme overlay staged via
//!   `contribution_publish` and atomically OCC-published);
//! - module-emitted intents are capability-checked (R-27: subject to the
//!   standard capability intersection) and produce canonical domain facts
//!   (e.g. `user_message`), never gestures;
//! - fault classes (R-27): composition failure → staleness banner on the
//!   last-valid UI; runtime component fault → kernel placeholder + degraded;
//!   kernel render fault → kernel fallback UI (safe mode).

use std::io;

use kanbei_capabilities::{Capability, Principal};
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
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

/// The session-side UI host: one bound UI generation, its opaque reducer
/// state, and the kernel-owned interaction state (focus, decoder, classifier,
/// theme, last frame).
pub struct UiHost {
    pub component: String,
    pub generation: u64,
    state: Value,
    /// Runtime component fault flag (R-27 fault class 2): the kernel renders
    /// a placeholder until a successful reduce clears it.
    pub degraded: bool,
    pub last_error: Option<String>,
    /// Composition staleness banner (R-27 fault class 1).
    pub staleness: Option<String>,
    /// Kernel safe mode: fallback UI, module input dropped (R-27).
    pub safe_mode: bool,
    /// Intents dropped by the capability intersection.
    pub denied_intents: u64,
    /// Intents the last reduce returned, awaiting capability intersection.
    pending_intents: Vec<UiIntent>,
    pub focus: kanbei_ui::FocusModel,
    classifier: KeyClassifier,
    decoder: InputDecoder,
    theme: Theme,
    last_tree: Option<SemanticTree>,
    last_frame: Option<TerminalFrame>,
    last_diff: FrameDiff,
    size: (u16, u16),
    pub viewport_top: usize,
    pub last_status: String,
}

/// What one `ui_handle_input` pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiOutcome {
    pub intents_applied: usize,
    pub denied: u64,
    pub degraded: bool,
    pub staleness: Option<String>,
    pub safe_mode: bool,
    pub repaint: bool,
}

impl Default for UiOutcome {
    fn default() -> Self {
        UiOutcome {
            intents_applied: 0,
            denied: 0,
            degraded: false,
            staleness: None,
            safe_mode: false,
            repaint: false,
        }
    }
}

impl UiHost {
    fn bind(component: String, generation: u64, theme: Theme) -> Self {
        UiHost {
            component,
            generation,
            state: Value::Null,
            degraded: false,
            last_error: None,
            staleness: None,
            safe_mode: false,
            denied_intents: 0,
            pending_intents: Vec::new(),
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
        }
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
    /// canonical `composition_changed`), then bind the UI host to the
    /// composition's root-scope UI mount. Any failure retains the last-valid
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
        let epoch = self.activate_ui(manifest)?;
        let generation = self.ui_host.as_ref().map(|u| u.generation).unwrap_or(0);
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

    /// Bind (or rebind) the UI host to the composition's root-scope UI mount
    /// and the generation that mounted it.
    fn rebind_ui(&mut self, _generation: u64) -> Result<(), SessionError> {
        let Some(manager) = self.modules.as_ref() else {
            return Ok(());
        };
        let root = kanbei_services::ScopePath(vec!["root".into()]);
        let mount = self
            .registry
            .snapshot()
            .into_iter()
            .find_map(|c| match c.kind {
                kanbei_scopes::contrib::ContributionKind::UiMount(m) if c.scope == root => {
                    Some(m)
                }
                _ => None,
            });
        let Some(mount) = mount else {
            return Ok(());
        };
        let Some(resolved) = manager.ui_generation(&mount.component) else {
            return Ok(());
        };
        let mut theme = Theme::default_theme();
        if let Some(overlay) = self.registry.theme_overlay(&root, &mount.name) {
            let _ = theme.apply_overlay(&overlay.overlay);
        }
        self.ui_host = Some(UiHost::bind(mount.component, resolved, theme));
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
    /// focus navigation, module reduce, intents, render, diff. Returns the
    /// outcome; the frame is available via `ui().last_frame()` and presented
    /// by [`Session::ui_present`].
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
        if let Some(host) = self.ui_host.as_ref() {
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
    /// and activation go to the module reducer.
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
                // Enter on a focused button resolves to an activation event.
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

    fn ui_focus_move(&mut self, dir: FocusDirection) {
        let Some(host) = self.ui_host.as_mut() else {
            return;
        };
        if let Some(tree) = host.last_tree.clone() {
            host.focus.move_focus(&tree, dir);
        }
    }

    /// Call the UI generation's reducer. On any fault the module is marked
    /// degraded and the kernel renders a placeholder (R-27 fault class 2).
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
        let payload = match event.kind {
            UiEventKind::Char(c) => json!({
                "entry": "ui_reduce",
                "state": host.state,
                "event": {"kind": "char", "text": c.to_string()},
            }),
            UiEventKind::Backspace => json!({
                "entry": "ui_reduce",
                "state": host.state,
                "event": {"kind": "backspace"},
            }),
            UiEventKind::Enter => json!({
                "entry": "ui_reduce",
                "state": host.state,
                "event": {"kind": "enter"},
            }),
            UiEventKind::Activate(id) => json!({
                "entry": "ui_reduce",
                "state": host.state,
                "event": {"kind": "activate", "node": id},
            }),
        };
        let out = match manager.call_generation(host.generation, &payload.to_string()) {
            Ok(out) => out,
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(e.to_string());
                host.last_tree = Some(fallback::placeholder_tree(&host.component, &e.to_string()));
                return;
            }
        };
        let v: Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(format!("ui_reduce: invalid result JSON: {e}"));
                host.last_tree = Some(fallback::placeholder_tree(
                    &host.component,
                    host.last_error.as_deref().unwrap_or("reduce failed"),
                ));
                return;
            }
        };
        host.state = v.get("state").cloned().unwrap_or(Value::Null);
        host.pending_intents = v
            .get("intents")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(UiIntent::from_json).collect())
            .unwrap_or_default();
        host.degraded = false;
        host.last_error = None;
    }

    /// Apply the pending module intents through the capability intersection.
    fn apply_ui_intents(&mut self) -> Result<usize, SessionError> {
        let intents = self
            .ui_host
            .as_mut()
            .map(|u| std::mem::take(&mut u.pending_intents))
            .unwrap_or_default();
        let mut applied = 0;
        for intent in intents {
            let generation = self.ui_host.as_ref().map(|u| u.generation).unwrap_or(0);
            let principal = Principal {
                session: self.session_id(),
                generation,
                run: None,
            };
            let want = match &intent {
                UiIntent::SubmitText { .. } => Capability::new("session".into(), vec!["append".into()]),
                UiIntent::CancelRun => Capability::new("session".into(), vec!["cancel".into()]),
            };
            let allowed = self
                .broker
                .check(&principal, &want, self.broker.policy_version())
                .is_ok();
            if !allowed {
                if let Some(host) = self.ui_host.as_mut() {
                    host.denied_intents += 1;
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
        Ok(applied)
    }

    /// Re-render the module tree into a frame + diff against the last frame
    /// (kernel-owned rendering; the module produced only tree data).
    pub fn ui_render_frame(&mut self) -> Result<(), SessionError> {
        let safe_mode = self.ui_host.as_ref().map(|h| h.safe_mode).unwrap_or(true);
        let degraded = self.ui_host.as_ref().map(|h| h.degraded).unwrap_or(true);
        let tree = if safe_mode {
            // Kernel fallback UI (R-27 fault class 3): module input dropped.
            self.ui_host
                .as_mut()
                .and_then(|h| h.last_tree.clone())
                .unwrap_or_else(|| fallback::FallbackUi::new("safe mode").tree())
        } else if degraded {
            self.ui_host
                .as_mut()
                .and_then(|h| h.last_tree.clone())
                .unwrap_or_else(|| fallback::placeholder_tree("workbench", "module degraded"))
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

    /// Call the UI generation's render entry, validate the tree
    /// (accessibility pass is kernel-owned), and store it. Returns None on
    /// any fault (placeholder + degraded set).
    fn ui_render_module_tree(&mut self) -> Option<SemanticTree> {
        self.fault(FaultPoint::BeforeUiRender);
        let result = self.ui_render_module_tree_inner();
        self.fault(FaultPoint::AfterUiRender);
        result
    }

    fn ui_render_module_tree_inner(&mut self) -> Option<SemanticTree> {
        let Some(host) = self.ui_host.as_mut() else {
            return None;
        };
        let Some(manager) = self.modules.as_ref() else {
            return None;
        };
        let payload = json!({ "entry": "ui_render", "state": host.state });
        let out = match manager.call_generation(host.generation, &payload.to_string()) {
            Ok(out) => out,
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(e.to_string());
                host.last_tree = Some(fallback::placeholder_tree(&host.component, &e.to_string()));
                return None;
            }
        };
        let v: Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(format!("ui_render: invalid result JSON: {e}"));
                host.last_tree = Some(fallback::placeholder_tree(
                    &host.component,
                    host.last_error.as_deref().unwrap_or("render failed"),
                ));
                return None;
            }
        };
        let tree = match SemanticTree::from_json(&v) {
            Ok(tree) => tree,
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(format!("ui_render: invalid tree: {e}"));
                host.last_tree = Some(fallback::placeholder_tree(
                    &host.component,
                    host.last_error.as_deref().unwrap_or("render failed"),
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
            host.degraded = true;
            host.last_error = Some(format!("accessibility: {}", errors.join("; ")));
            host.last_tree = Some(fallback::placeholder_tree(
                &host.component,
                host.last_error.as_deref().unwrap_or("invalid tree"),
            ));
            return None;
        }
        host.last_tree = Some(tree.clone());
        host.degraded = false;
        host.last_error = None;
        Some(tree)
    }

    /// Push a kernel facts refresh to the module (e.g. after a canonical
    /// event the UI should reflect).
    pub fn ui_refresh(&mut self, last_outcome: &str) -> Result<(), SessionError> {
        let Some(host) = self.ui_host.as_mut() else {
            return Ok(());
        };
        if host.degraded || host.safe_mode {
            return Ok(());
        }
        let Some(manager) = self.modules.as_ref() else {
            return Ok(());
        };
        let payload = json!({
            "entry": "ui_reduce",
            "state": host.state,
            "event": {"kind": "refresh", "facts": {"last_outcome": last_outcome}},
        });
        match manager.call_generation(host.generation, &payload.to_string()) {
            Ok(out) => {
                if let Ok(v) = serde_json::from_str::<Value>(&out) {
                    host.state = v.get("state").cloned().unwrap_or(host.state.clone());
                }
            }
            Err(e) => {
                host.degraded = true;
                host.last_error = Some(e.to_string());
                host.last_tree = Some(fallback::placeholder_tree(&host.component, &e.to_string()));
            }
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
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
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
            let fallback_tree = fallback::FallbackUi::new(&format!("kernel render fault: {e}")).tree();
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
            let output = render(&ctx).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
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
