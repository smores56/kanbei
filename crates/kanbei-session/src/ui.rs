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
//! lays out and presents the composed tree (decision 22) and owns only its
//! fallback/staleness/safe-mode chrome (R-27) — the module authors the shell's
//! status/header/input rows.

use std::io;

use kanbei_capabilities::{Capability, Principal};
use kanbei_context::OpenLoop;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_modules::ModuleManager;
use kanbei_scopes::contrib::{Keybinding, KeyContext};
use kanbei_transcript::{CollapseOverrides, TranscriptView};
use kanbei_ui::accessibility;
use kanbei_ui::fallback;
use kanbei_ui::focus::{FocusDirection, InputClass, KeyClassifier, ReservedAction};
use kanbei_ui::frame::{RenderContext, render};
use kanbei_ui::input::{InputDecoder, InputEvent, UiEvent, UiEventKind};
use kanbei_ui::theme::Theme;
use kanbei_ui::tree::{NodeKind, SemanticTree};
use kanbei_ui::{RenderOutput, Terminal};
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
    /// Re-open or re-collapse a settled turn's working segment. Presentation
    /// only (decisions 9/32): the kernel applies it to the session-local
    /// collapse overrides, so the projection stays a pure function of the
    /// committed envelopes.
    ToggleCollapse { turn: usize },
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
            Some("toggle_collapse") => v
                .get("turn")
                .and_then(Value::as_u64)
                .map(|turn| UiIntent::ToggleCollapse {
                    turn: turn as usize,
                }),
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
    /// The scope the mount's contribution was published in (root scope for a
    /// bound mount); a keymap binding is "owned" by a degraded mount when its
    /// origin scope matches.
    pub scope: kanbei_services::ScopePath,
    /// The composite region this mount renders into (canonical slots:
    /// `main`, `status`, `header`, `composer`, `aux`; `None` is the default
    /// `main` and is normalized by the registry at publish).
    pub slot: String,
    /// The mount's contribution name (unique per root scope).
    pub name: String,
    /// The UI component entry (the generation's `kb_hot` ui entry).
    pub component: String,
    pub generation: u64,
    /// The stable module id that owns this mount (the generation's manifest
    /// module id). Keybinding ownership attribution: a degraded mount disables
    /// only bindings published by its own module.
    pub module_id: Option<Id128>,
    /// The mount's last validated tree with ORIGINAL ids (the composite view
    /// prefixes them). `None` until the first render.
    pub tree: Option<SemanticTree>,
    /// Last focused node id within this mount (original ids); restored when
    /// Tab cycles back into the mount.
    pub focus: Option<String>,
    /// Session-local collapse overrides for THIS mount only (decision 32
    /// amendment): a mount's `toggle_collapse` intent is presentation-only and
    /// never reaches another mount's overrides or the kernel's session-global
    /// overrides.
    pub collapse: CollapseOverrides,
    /// Whether the mount's manifest origin is trusted (`ModuleOrigin::
    /// is_trusted`). An untrusted mount receives an EMPTY `context.transcript`
    /// (decision 32 amendment): prompts, model output and tool results are
    /// not exposed to repo/agent/install-supplied modules.
    pub trusted: bool,
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
    /// The mount's contribution may have changed: the next composition
    /// rebuilds the composite (decision 22 event-driven composition). A
    /// non-degraded dirty mount re-invokes the guest `ui_render`; a degraded
    /// one is recomposed from its preserved tree with no guest call. Cleared
    /// at the composition attempt so a repaint never re-enters the guest and
    /// a fault is not retried per frame. Set on binding, reduce, refresh,
    /// resize, and reduce/refresh faults (which swap in a placeholder).
    dirty: bool,
    /// The render context JSON handed to this mount's last composition. A
    /// change to ANY context component (transcript, status, size, focus,
    /// selection, viewport, or this mount's own collapse overrides) marks the
    /// mount dirty (decision 32 amendment). Per mount because the transcript
    /// is trust-gated.
    last_context: Option<Value>,
    /// Guest `ui_render` invocations since binding. Test observability for
    /// "was the guest on the frame loop?" (decision 22): a pure repaint must
    /// leave this unchanged.
    pub render_calls: u64,
}

impl BoundMount {
    fn new(
        scope: kanbei_services::ScopePath,
        slot: String,
        name: String,
        component: String,
        generation: u64,
        module_id: Option<Id128>,
        trusted: bool,
    ) -> Self {
        BoundMount {
            scope,
            slot,
            name,
            component,
            generation,
            module_id,
            tree: None,
            focus: None,
            collapse: CollapseOverrides::new(),
            trusted,
            reducer_state: Value::Null,
            degraded: false,
            last_error: None,
            denied_intents: 0,
            pending_intents: Vec::new(),
            dirty: true,
            last_context: None,
            render_calls: 0,
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
    last_frame: Option<RenderOutput>,
    size: (u16, u16),
    pub viewport_top: usize,
    /// The kernel-owned selection pointer (decision 22): the last non-input
    /// node focus landed on, exposed to the module through the render context
    /// so the shell can highlight the selected row. Native: moving it never
    /// re-enters Wasm.
    pub selection: Option<String>,
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
    /// The kernel reserved Ctrl-Z (Suspend) was pressed: the driver should
    /// suspend the UI to the shell (decision 29).
    pub suspend: bool,
    /// A `submit_text` intent was applied: the driver should now drive the
    /// triggered run to quiescence.
    pub submitted: bool,
    /// The kernel's `quit` binding won: the driver should shut down.
    pub quit: bool,
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
            size: (24, 80),
            viewport_top: 0,
            selection: None,
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

    /// The last rendered surface (the kernel's canonical render output).
    pub fn last_frame(&self) -> Option<&RenderOutput> {
        self.last_frame.as_ref()
    }
}

/// The built-in UI generation's deterministic module id: derived from its
/// immutable source the same way the built-in config layer derives its own
/// ([`crate::builtin_config`]), so re-activation addresses the same identity
/// (R-08 stable ModuleId + immutable content hash) instead of minting a fresh
/// id each time.
pub fn builtin_ui_module_id() -> Id128 {
    crate::builtin_config::config_module_id(kanbei_ui::BUILTIN_UI_SOURCE.as_bytes())
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
            schema: kanbei_modules::PACKAGE_SCHEMA,
            module_id: builtin_ui_module_id(),
            origin: ModuleOrigin::Builtin,
            trust_class: kanbei_capabilities::TrustClass::Builtin,
            scope: kanbei_services::ScopePath(vec!["root".into()]),
            deps: Vec::new(),
            capabilities: Vec::new(),
            source: kanbei_ui::BUILTIN_UI_SOURCE.to_string(),
            state_schema: None,
            state_key: None,
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
        // T9: a composition change rebinds the hook set too (same choke point
        // as the UI mounts below).
        self.rebind_hooks();
        let Some(manager) = self.modules.as_ref() else {
            return Ok(());
        };
        let root = kanbei_services::ScopePath(vec!["root".into()]);
        let mut mounts: Vec<(String, String, String, kanbei_services::ScopePath)> = self
            .registry
            .snapshot()
            .into_iter()
            .filter_map(|c| match c.kind {
                kanbei_scopes::contrib::ContributionKind::UiMount(m) if c.scope == root => Some((
                    m.slot.unwrap_or_else(|| "main".to_string()),
                    m.name,
                    m.component,
                    c.scope,
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
        for (slot, name, component, scope) in mounts {
            let Some(generation) = manager.ui_generation(&component) else {
                continue;
            };
            if let Some(overlay) = self.registry.theme_overlay(&root, &name) {
                let _ = theme.apply_overlay(&overlay.overlay);
            }
            let module_id = manager.generation_module_id(generation);
            let trusted = manager
                .generation_origin(generation)
                .is_some_and(ModuleOrigin::is_trusted);
            bound.push(BoundMount::new(
                scope,
                slot,
                name,
                component,
                generation,
                module_id,
                trusted,
            ));
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

    /// A snapshot of the resolved keybindings, for a caller outside the session
    /// that must classify a key against the same keymap the kernel routes with
    /// (the CLI relaying an approval/cancel decision while a turn blocks the
    /// session in the approval rendezvous).
    pub fn ui_keybindings(&self) -> Vec<Keybinding> {
        self.registry.keybindings()
    }

    /// The kernel status text for the shell's status/header contribution.
    pub fn ui_status_text(&self) -> String {
        if self.ui_host.as_ref().is_none_or(|u| u.safe_mode) {
            return "safe mode".to_string();
        }
        if let Some(parked) = self.approvals.back() {
            // An approval gate outranks run state: the status line names the
            // action the user must approve or deny.
            return format!("approval: {}", parked.approval.action);
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
        // Layer-2: a user message is an open loop until a run completes the
        // goal it expresses (R-12/F-S5). Bounded: the loop list is cleared at
        // each CompletedGoal.
        self.open_loops.push(OpenLoop {
            id: receipt.last_seq.to_string(),
            text: text.to_string(),
            created_event: receipt.last_seq,
            sensitivity: "internal".into(),
        });
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
                host.decoder.feed(bytes)
            }
            None => return Ok(outcome),
        };
        for event in events {
            let class = self
                .ui_host
                .as_mut()
                .map(|host| {
                    let modal_active = host.focus.boundary().is_some();
                    host.classifier.classify(&event, modal_active)
                })
                .unwrap_or(InputClass::Forward);
            match class {
                InputClass::Reserved(ReservedAction::Suspend) => {
                    outcome.suspend = true;
                }
                InputClass::Reserved(ReservedAction::SafeModeChord) => {
                    self.enter_ui_safe_mode()?;
                    outcome.safe_mode = true;
                    outcome.repaint = true;
                }
                InputClass::Reserved(ReservedAction::ModalEscape) => {
                    // Reserved Escape leaves the active modal boundary; the
                    // module never sees it (modal closure is not canonical).
                    if let Some(host) = self.ui_host.as_mut()
                        && let Some(tree) = host.last_tree.clone()
                    {
                        host.focus.escape_modal(&tree);
                        host.focus.revalidate(&tree);
                    }
                    outcome.repaint = true;
                }
                InputClass::Consumed => {}
                InputClass::Forward => {
                    // Decision 29: reserved classification wins first; then the
                    // winning context-matched binding (if any) routes its action
                    // id as a typed command intent through the normal reduce
                    // path; only unbound keys fall through to raw forwarding.
                    if let Some(binding) = self.ui_binding_action(&event) {
                        match binding.action.as_str() {
                            // The built-in layer's default bindings restore the
                            // pre-T10 kernel behaviors: `cancel_run` cancels the
                            // active run, `quit` shuts the driver down, `repaint`
                            // forces a full repaint. All stay remappable (a
                            // binding can target other ids).
                            "cancel_run" => {
                                if self.scheduler.active_run().is_some() {
                                    let _ = self.cancel_active_run()?;
                                }
                                outcome.repaint = true;
                            }
                            "quit" => outcome.quit = true,
                            "repaint" => outcome.repaint = true,
                            // Approval-gate keys (decision 8): the built-in
                            // layer binds `approve`/`deny` under the modal
                            // context, so a parked approval is decided by the
                            // SAME keymap path as every other key — not a
                            // parallel Rust mapping. The rendezvous itself
                            // stays a session/kernel concern. With nothing
                            // parked the actions fall through to the module
                            // (a dialog may define its own approve/deny).
                            "approve" | "deny" if !self.approvals.is_empty() => {
                                self.resolve_pending_approval(binding.action == "approve")?;
                                outcome.repaint = true;
                            }
                            _ => {
                                // Route by OWNERSHIP first: the action id is
                                // its owner module's namespace; only fall back
                                // to the focused mount, then to fan-out.
                                self.ui_reduce_command(&binding.action, binding.owner)?;
                                self.apply_ui_intents(&mut outcome)?;
                            }
                        }
                    } else {
                        self.ui_forward(&event, &mut outcome)?;
                    }
                }
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

    /// Flush a partial escape/UTF-8 sequence at terminal EOF/close. The
    /// decoder only drops the leftover bytes (no event survives
    /// sanitization), so this is a close-path no-op in effect; it must NOT run
    /// after every read — flushing mid-burst would exit paste mode and drop a
    /// sequence split across reads.
    pub fn ui_flush_input(&mut self) {
        if let Some(host) = self.ui_host.as_mut() {
            let _ = host.decoder.finish();
        }
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

    /// The winning binding and its owner for `event` under the current UI
    /// context, if any binding matches (decision 29). Reserved keys are
    /// classified before this and never consult the binding table. Bindings
    /// owned by a degraded mount are skipped so a bound key falls through to
    /// forwarding instead of being silently swallowed by a dead reducer.
    fn ui_binding_action(&self, event: &InputEvent) -> Option<WinningBinding> {
        let key = event.key_name()?;
        let host = self.ui_host.as_ref()?;
        let ctx = KeyContext {
            // A parked approval is a modal gate (decision 8): its
            // approve/deny bindings are live under the `modal` context.
            modal: host.focus.boundary().is_some() || !self.approvals.is_empty(),
            overlay: host
                .last_tree
                .as_ref()
                .is_some_and(|t| t.overlay_present()),
        };
        let (_, binding) = self.registry.keymap_winner(&key, ctx)?;
        if binding_is_degraded(binding, host) {
            return None;
        }
        Some(WinningBinding {
            action: binding.action.clone(),
            owner: binding.owner,
        })
    }

    /// Resolve the newest parked approval through the keymap's
    /// `approve`/`deny` action. No-op when nothing is parked (the action is
    /// still the binding's to own).
    fn resolve_pending_approval(&mut self, approve: bool) -> Result<(), SessionError> {
        if let Some(digest) = self.approvals.back().map(|p| p.approval.digest) {
            self.resolve_approval(&digest, approve)?;
        }
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
                let kind = match event.to_ui() {
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
                        Some(n) if n.kind() == NodeKind::Button => UiEventKind::Activate(id.to_string()),
                        _ => kind,
                    },
                    _ => kind,
                };
                self.ui_reduce(UiEvent::user(kind))?;
                self.apply_ui_intents(outcome)?;
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
        // Native selection (decision 22): the selection pointer follows focus
        // onto a non-input node; the shell reads it from the render context.
        // Focus on an input leaves the last selection intact.
        let selectable = host
            .focus
            .focused
            .as_deref()
            .and_then(|id| tree.node(id))
            .is_some_and(|n| n.kind() != NodeKind::Input);
        if selectable {
            host.selection = host.focus.focused.clone();
        }
    }

    /// Call every mount's reducer with the event (M8 fan-out): the kernel
    /// delivers to all mounts in slot order, each with its own state; the
    /// event carries the focused mount's slot as a `target` hint so a
    /// reducer can ignore non-target events. A fault degrades only that
    /// mount (placeholder subtree); the others keep working.
    fn ui_reduce(&mut self, event: UiEvent) -> Result<(), SessionError> {
        self.fault(FaultPoint::BeforeUiReduce);
        self.ui_reduce_inner(event, ReduceTarget::Fanout);
        self.fault(FaultPoint::AfterUiReduce);
        Ok(())
    }

    /// Deliver a binding's command to its OWNER mount (the module that
    /// published the binding), else to the focused mount, else fan out — never
    /// silently drop. The action id is the owner module's namespace, so a
    /// victim reducer must not act on it.
    fn ui_reduce_command(
        &mut self,
        action: &str,
        owner: Option<Id128>,
    ) -> Result<(), SessionError> {
        let target = {
            let host = self.ui_host.as_ref();
            // Prefer the binding's owner; a config/global binding with no
            // mount of its own falls back to the focused mount.
            let focus_index = host.and_then(Self::focused_mount_index);
            let owner_index = owner.and_then(|mid| {
                host.and_then(|h| h.mounts.iter().position(|m| m.module_id == Some(mid)))
            });
            match owner_index.or(focus_index) {
                Some(i) => ReduceTarget::Index(i),
                None => ReduceTarget::Fanout,
            }
        };
        self.fault(FaultPoint::BeforeUiReduce);
        self.ui_reduce_inner(UiEvent::user(UiEventKind::Command(action.to_string())), target);
        self.fault(FaultPoint::AfterUiReduce);
        Ok(())
    }

    /// The focused mount's index, or the sole mount's index when the focused
    /// id is unresolvable (single-mount ids are unprefixed, M5 byte-identical
    /// trees).
    fn focused_mount_index(host: &UiHost) -> Option<usize> {
        host.focus
            .focused
            .as_deref()
            .and_then(SemanticTree::split_composite_id)
            .map(|(i, _)| i)
            .filter(|i| *i < host.mounts.len())
            .or_else(|| (host.mounts.len() == 1).then_some(0))
    }

    fn ui_reduce_inner(&mut self, event: UiEvent, target: ReduceTarget) {
        let Some(host) = self.ui_host.as_mut() else {
            return;
        };
        let Some(manager) = self.modules.as_ref() else {
            return;
        };
        // Fan-out still carries the focused mount's slot as an advisory
        // `target` hint; an explicit target restricts delivery to one mount.
        let (target_index, target_only) = match target {
            ReduceTarget::Fanout => (Self::focused_mount_index(host), false),
            ReduceTarget::Index(i) => (Some(i), true),
        };
        let target = target_index.map(|i| host.mounts[i].slot.clone());
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
            UiEventKind::Command(action) => json!({ "kind": "command", "action": action }),
        };
        let mut event_value = event_value;
        if let Some(target) = target {
            event_value["target"] = json!(target);
        }
        for (i, mount) in host.mounts.iter_mut().enumerate() {
            if target_only && Some(i) != target_index {
                continue;
            }
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
                    mount.dirty = true;
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
                    mount.dirty = true;
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
            // The reducer may have changed state: recompose this mount on the
            // next frame (decision 22).
            mount.dirty = true;
        }
    }

    /// Apply the pending intents of every mount through the capability
    /// intersection, per mount with THAT mount's generation grants (M8
    /// capability isolation: a mount without a grant has its intent denied
    /// while another mount's identical intent applies). Accepted intents
    /// apply in slot order. Denied intents are dropped and counted per
    /// mount, never canonical. The presentation-only `toggle_collapse` is
    /// kernel-local (decision 32) and skips the capability intersection.
    fn apply_ui_intents(&mut self, outcome: &mut UiOutcome) -> Result<(), SessionError> {
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
        for (mount_index, generation, intents) in intents {
            for intent in intents {
                // A collapse toggle only changes session-local presentation:
                // the kernel applies it under the view build, so it carries no
                // capability (decisions 9/32).
                if let UiIntent::ToggleCollapse { turn } = intent {
                    // Presentation-only and mount-scoped (decision 32
                    // amendment): toggle the EMITTING mount's own overrides,
                    // never another mount's or the kernel's session-global set.
                    if let Some(host) = self.ui_host.as_mut()
                        && let Some(mount) = host.mounts.get_mut(mount_index)
                    {
                        mount.collapse.toggle(turn);
                        mount.dirty = true;
                    }
                    outcome.intents_applied += 1;
                    continue;
                }
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
                    UiIntent::ToggleCollapse { .. } => unreachable!("handled above"),
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
                        outcome.submitted = true;
                        outcome.intents_applied += 1;
                        let _ = self.ui_refresh("user message committed");
                    }
                    UiIntent::CancelRun => {
                        if self.scheduler.active_run().is_some() {
                            let _ = self.cancel_active_run()?;
                        }
                        outcome.intents_applied += 1;
                    }
                    UiIntent::ToggleCollapse { .. } => unreachable!("handled above"),
                }
            }
        }
        Ok(())
    }

    /// Mark every non-degraded mount for recomposition (a context-level change
    /// the per-mount reducer state cannot see: transcript, overrides, size).
    pub(crate) fn mark_ui_dirty(&mut self) {
        if let Some(host) = self.ui_host.as_mut() {
            for mount in host.mounts.iter_mut() {
                mount.dirty = true;
            }
        }
    }

    /// Replace the session-local collapse overrides (decision 9): ephemeral
    /// presentation applied when the render context's transcript view is
    /// built. Never canonical, never persisted, so resume is identical.
    pub fn set_transcript_overrides(&mut self, overrides: CollapseOverrides) {
        self.transcript_overrides = overrides;
        self.mark_ui_dirty();
    }

    /// Re-open or re-collapse one turn's working segment (the module's
    /// `toggle_collapse` intent routes here too).
    pub fn toggle_transcript_collapse(&mut self, turn: usize) {
        self.transcript_overrides.toggle(turn);
        self.mark_ui_dirty();
    }

    /// Re-render the composite of the mount trees into the canonical render
    /// surface (kernel-owned rendering; the modules produced only tree data).
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
        let Some(host) = self.ui_host.as_mut() else {
            return Ok(());
        };
        host.sync_summary();
        // The tree is authoritative for modal containment: enter/leave the
        // topmost modal boundary and clamp focus before rendering.
        host.focus.sync_modal_boundary(&tree);
        let ctx = RenderContext {
            tree: &tree,
            theme: &host.theme,
            focus: &host.focus,
            size,
            selection: host.selection.as_deref(),
            staleness: host.staleness.as_deref(),
        };
        let output = render(&ctx).map_err(|e| SessionError::InvalidInput(e.to_string()))?;
        host.viewport_top = output.viewport_top;
        host.focus.viewport_top = output.viewport_top;
        host.last_frame = Some(output);
        Ok(())
    }

    /// Compose the mount trees into one synthetic root (slot order = child
    /// order). Event-driven (decision 22): a mount with a clear dirty flag
    /// contributes its cached last-valid tree without a guest call, so a
    /// repaint never re-enters Wasm; a dirty mount is re-rendered and
    /// kernel-validated, and a fault preserves its last-valid tree and
    /// degrades only that mount. Returns None only when there is nothing to
    /// render.
    fn ui_render_module_tree(&mut self) -> Option<SemanticTree> {
        self.fault(FaultPoint::BeforeUiRender);
        let result = self.ui_render_module_tree_inner();
        self.fault(FaultPoint::AfterUiRender);
        result
    }

    fn ui_render_module_tree_inner(&mut self) -> Option<SemanticTree> {
        // The render context is kernel-owned (decision 32) and PER MOUNT: the
        // transcript is trust-gated and each mount carries its own collapse
        // overrides (32 amendments), so both are built before the mutable host
        // borrow. The status/size/focus/selection/viewport pieces are shared.
        let global = self.transcript_overrides.clone();
        let status = self.ui_status_text();
        let (size, focus, selection, viewport_top, scopes) = {
            let host = self.ui_host.as_ref()?;
            (
                host.size,
                host.focus.focused.clone(),
                host.selection.clone(),
                host.viewport_top as u32,
                host.mounts
                    .iter()
                    .map(|m| (m.collapse.clone(), m.trusted))
                    .collect::<Vec<_>>(),
            )
        };
        let (rows, cols) = size;
        let contexts: Vec<Value> = scopes
            .iter()
            .map(|(collapse, trusted)| {
                let transcript = if *trusted {
                    let mut overrides = global.clone();
                    // The session-global overrides (CLI/test API) layer UNDER
                    // the mount's own intent-driven overrides.
                    overrides.union_with(collapse);
                    self.transcript_view(&overrides)
                } else {
                    // Untrusted origin: status/size/navigation, no transcript.
                    TranscriptView::default()
                };
                json!({
                    "transcript": serde_json::to_value(&transcript).unwrap_or(Value::Null),
                    "status": &status,
                    "size": { "cols": cols, "rows": rows },
                    "focus": &focus,
                    "selection": &selection,
                    "viewport_top": viewport_top,
                })
            })
            .collect();
        let manager = self.modules.as_ref()?;
        let host = self.ui_host.as_mut()?;
        // Event-driven composition (decision 22): a mount whose context is
        // unchanged since its last composition is cached natively, so a pure
        // repaint never re-enters Wasm. ANY context component change
        // (transcript/status/size/focus/selection/viewport/overrides) dirties
        // the mount that renders it (decision 32 amendment).
        let mut changed = false;
        for (mount, context) in host.mounts.iter_mut().zip(contexts.iter()) {
            if mount.last_context.as_ref() != Some(context) {
                mount.dirty = true;
                mount.last_context = Some(context.clone());
            }
            if !mount.dirty {
                continue;
            }
            mount.dirty = false;
            changed = true;
            if mount.degraded {
                // A degraded mount keeps its last tree (placeholder or
                // last-valid); the guest is not retried until a successful
                // reduce clears the flag.
                continue;
            }
            Self::render_mount(manager, mount, context);
        }
        if !changed && let Some(tree) = host.last_tree.as_ref() {
            return Some(tree.clone());
        }
        for mount in host.mounts.iter_mut() {
            if mount.tree.is_none() {
                mount.tree = Some(fallback::placeholder_tree(
                    &mount.component,
                    "render failed",
                ));
            }
        }
        let refs: Vec<(&str, &SemanticTree)> = host
            .mounts
            .iter()
            .filter_map(|m| m.tree.as_ref().map(|t| (m.slot.as_str(), t)))
            .collect();
        let composite = SemanticTree::compose(&refs);
        host.last_tree = Some(composite.clone());
        Some(composite)
    }

    /// Render one mount's tree through its generation and kernel-validate it
    /// (accessibility pass is kernel-owned, R-27), storing the validated tree
    /// on the mount. `context` is the kernel-owned read-only render context
    /// (decision 32). A fault degrades the mount but PRESERVES its last-valid
    /// tree: the composite keeps rendering the previous good tree until the
    /// mount's state changes again (decision 22 fault → last-valid). The
    /// caller supplies a placeholder only when the mount never had a valid
    /// tree.
    fn render_mount(manager: &ModuleManager, mount: &mut BoundMount, context: &Value) {
        mount.render_calls += 1;
        let payload = json!({
            "entry": "ui_render",
            "state": mount.reducer_state,
            "context": context,
        });
        let out = match manager.call_generation(mount.generation, &payload.to_string()) {
            Ok(out) => out,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(e.to_string());
                return;
            }
        };
        let v: Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(format!("ui_render: invalid result JSON: {e}"));
                return;
            }
        };
        let tree = match SemanticTree::from_json(&v) {
            Ok(tree) => tree,
            Err(e) => {
                mount.degraded = true;
                mount.last_error = Some(format!("ui_render: invalid tree: {e}"));
                return;
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
            return;
        }
        mount.tree = Some(tree);
        mount.degraded = false;
        mount.last_error = None;
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
                    // An explicit refresh forces a recomposition.
                    mount.dirty = true;
                }
                Err(e) => {
                    mount.degraded = true;
                    mount.last_error = Some(e.to_string());
                    mount.tree =
                        Some(fallback::placeholder_tree(&mount.component, &e.to_string()));
                    mount.dirty = true;
                }
            }
        }
        if let Some(host) = self.ui_host.as_mut() {
            host.sync_summary();
        }
        Ok(())
    }

    /// Present the last rendered surface to the kernel terminal boundary.
    /// `ui_present` owns the terminal: it takes the kernel fd boundary and
    /// paints the session's canonical [`RenderOutput`] through ratatui's
    /// diffing engine (no hand-rolled cell diff, no terminal-side session
    /// state). A write failure is a kernel render fault: the kernel fallback
    /// UI is rendered and presented instead (R-27 fault class 3).
    pub fn ui_present(&mut self, terminal: &mut dyn Terminal) -> io::Result<()> {
        match self.ui_host.as_mut() {
            Some(host) => {
                let size = terminal.size()?;
                if size != host.size {
                    host.size = size;
                    // A resize is a composition state change: the guest may
                    // lay out for the new surface (decision 22).
                    for mount in host.mounts.iter_mut() {
                        mount.dirty = true;
                    }
                }
            }
            None => return Ok(()),
        }
        self.ui_render_frame()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let frame = self
            .ui_host
            .as_ref()
            .expect("ui host present")
            .last_frame
            .clone()
            .expect("frame rendered");
        if let Err(e) = frame.present(terminal) {
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
                selection: None,
                staleness: host.staleness.as_deref(),
            };
            let output = render(&ctx).map_err(|e| io::Error::other(e.to_string()))?;
            host.last_frame = Some(output);
            host.last_frame
                .as_ref()
                .expect("fallback frame")
                .present(terminal)?;
        }
        Ok(())
    }
}

/// A winning binding plus the module that owns it (the command routing key).
struct WinningBinding {
    action: String,
    owner: Option<Id128>,
}

/// Where a reducer event is delivered.
#[derive(Clone, Copy)]
enum ReduceTarget {
    /// Every mount (raw forwarding); the event carries the focused mount's
    /// slot as an advisory hint.
    Fanout,
    /// Only the mount at this index.
    Index(usize),
}

/// Whether a winning binding is owned by a degraded mount. Attribution is by
/// MODULE identity: a binding is skipped only when the mount owned by the
/// module that published it is degraded, so one degraded module never
/// disables every binding sharing its scope. An unattributable binding
/// (`owner == None`) is never skipped — fail-safe: forward rather than
/// silently swallow input. Built-in/config bindings own no mount, so they are
/// never skipped (the keymap stays in the registry after a placeholder swap).
fn binding_is_degraded(binding: &Keybinding, host: &UiHost) -> bool {
    match binding.owner {
        Some(owner) => host
            .mounts
            .iter()
            .any(|m| m.degraded && m.module_id == Some(owner)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in UI generation is immutable content: its module id derives
    /// from the source, so re-activation/reopen address the same identity
    /// (R-08) rather than a freshly minted one.
    #[test]
    fn builtin_ui_module_id_is_deterministic() {
        assert_eq!(builtin_ui_module_id(), builtin_ui_module_id());
        assert_eq!(builtin_ui_module_id().to_string().len(), 21);
        assert_ne!(
            builtin_ui_module_id(),
            crate::builtin_config::builtin_config_module_id(),
            "distinct sources derive distinct identities"
        );
    }
}
