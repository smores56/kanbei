//! Kernel-owned focus/modal model (R-27). Focus always names a focusable,
//! non-disabled node of the current tree; after any tree change the model is
//! revalidated (clamped) so the invariant holds. The kernel reserves a
//! minimal interaction set (focus navigation, modal escape, repaint,
//! safe-mode entry) that modules cannot rebind.

use crate::tree::{Node, SemanticTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    Next,
    Prev,
    Up,
    Down,
    Left,
    Right,
}

/// Kernel-owned focus state. `caret` is a character offset into the focused
/// input node's content (the module owns the text; the kernel draws the
/// caret). `viewport_top` is the renderer's scroll hint, kept in sync with
/// the visible focused line by the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusModel {
    pub focused: Option<String>,
    pub caret: usize,
    pub viewport_top: usize,
    /// The active modal boundary: the topmost modal layer whose focusable
    /// descendants the ring is confined to. `None` = the whole tree.
    boundary: Option<String>,
    /// The boundary the user dismissed with modal escape; containment stays
    /// suspended for it until the topmost modal changes.
    escaped: Option<String>,
    /// Focus to restore when the active boundary is left.
    restore: Option<String>,
}

impl Default for FocusModel {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusModel {
    pub fn new() -> Self {
        FocusModel {
            focused: None,
            caret: 0,
            viewport_top: 0,
            boundary: None,
            escaped: None,
            restore: None,
        }
    }

    /// The active modal containment scope (the topmost modal layer's id), if
    /// any. `None` means the ring spans the whole tree.
    pub fn boundary(&self) -> Option<&str> {
        self.boundary.as_deref()
    }

    /// Restore the invariants against the current tree: focus names a
    /// focusable, non-disabled node INSIDE the active modal boundary; caret is
    /// clamped to the focused input's content length.
    pub fn revalidate(&mut self, tree: &SemanticTree) {
        self.sync_boundary(tree);
        match &self.focused {
            Some(id) if self.in_scope(tree, id) => {}
            _ => {
                self.focused = self.ring(tree).first().map(|n| n.id.clone());
                self.caret = 0;
            }
        }
        if let Some(node) = self.focused_node(tree) {
            self.clamp_caret(node);
        }
    }

    /// Reconcile the modal containment scope with a freshly rendered tree
    /// WITHOUT inventing a focus when none exists yet: entering a boundary
    /// pulls focus inside, and a vanished or out-of-scope focused id clamps.
    /// Keeps the module-driven Enter semantics while nothing is focused.
    pub fn sync_modal_boundary(&mut self, tree: &SemanticTree) {
        self.sync_boundary(tree);
        let needs_clamp = match &self.focused {
            Some(id) => !self.in_scope(tree, id),
            None => self.boundary.is_some(),
        };
        if needs_clamp {
            self.focused = self.ring(tree).first().map(|n| n.id.clone());
            self.caret = 0;
        }
        if let Some(node) = self.focused_node(tree) {
            self.clamp_caret(node);
        }
    }

    fn clamp_caret(&mut self, node: &Node) {
        if node.kind() == crate::NodeKind::Input {
            self.caret = self.caret.min(node.content().chars().count());
        } else {
            self.caret = 0;
        }
    }

    /// Reconcile the containment scope with the tree: enter the new topmost
    /// modal boundary, leave one that disappeared (restoring the remembered
    /// focus), or switch between them.
    fn sync_boundary(&mut self, tree: &SemanticTree) {
        let top = tree.modal_boundary().map(|n| n.id.clone());
        // A dismissal is scoped to the exact modal it was issued for.
        if self.escaped != top {
            self.escaped = None;
        }
        let active = match &top {
            Some(id) if self.escaped.as_deref() != Some(id.as_str()) => Some(id.clone()),
            _ => None,
        };
        if active == self.boundary {
            return;
        }
        match (active, self.boundary.take()) {
            // Entering: remember the outside focus to restore later.
            (Some(id), None) => {
                self.restore = self.focused.clone();
                self.boundary = Some(id);
            }
            // Leaving: restore the remembered focus (revalidated by caller).
            (None, Some(_)) => {
                if let Some(restore) = self.restore.take()
                    && tree.is_focusable(&restore)
                {
                    self.focused = Some(restore);
                    self.caret = 0;
                }
            }
            // Switching boundaries: restore only a focus from outside the new
            // boundary.
            (Some(id), Some(_)) => {
                if !self.in_boundary(tree, &id, self.focused.as_deref()) {
                    self.restore = self.focused.clone();
                }
                self.boundary = Some(id);
            }
            (None, None) => {}
        }
    }

    /// The focus ring for the current containment scope: the whole tree, or
    /// the focusable descendants of the active modal boundary.
    pub fn ring<'a>(&self, tree: &'a SemanticTree) -> Vec<&'a Node> {
        match self.boundary.as_deref() {
            Some(boundary) => tree
                .subtree(boundary)
                .into_iter()
                .filter(|n| n.is_focusable())
                .collect(),
            None => tree.focusable(),
        }
    }

    /// Dismiss the active modal boundary (kernel-reserved Escape): the ring
    /// spans the whole tree again and focus returns to where it was before
    /// containment.
    pub fn escape_modal(&mut self, tree: &SemanticTree) {
        if let Some(top) = tree.modal_boundary() {
            self.escaped = Some(top.id.clone());
        }
    }

    fn in_scope(&self, tree: &SemanticTree, id: &str) -> bool {
        match self.boundary.as_deref() {
            Some(boundary) => self.in_boundary(tree, boundary, Some(id)),
            None => tree.is_focusable(id),
        }
    }

    fn in_boundary(&self, tree: &SemanticTree, boundary: &str, id: Option<&str>) -> bool {
        let Some(id) = id else {
            return false;
        };
        tree.subtree(boundary)
            .into_iter()
            .any(|n| n.id == id && n.is_focusable())
    }

    /// Move focus through the focusable ring. Left/Right move the caret when
    /// the focused node is an input and are otherwise no-ops.
    pub fn move_focus(&mut self, tree: &SemanticTree, dir: FocusDirection) {
        self.revalidate(tree);
        let ring = self.ring(tree);
        self.move_ring(dir, ring);
    }

    /// Move focus through the ring RESTRICTED to the subtree of `root_id`
    /// (M8: Up/Down stay within the focused mount's subtree of the composite
    /// tree). When the focused node is outside the boundary the ring
    /// position resolves to its start, like a fresh entry. The boundary id
    /// must be a composite id (see `SemanticTree::compose`).
    pub fn move_focus_within(&mut self, tree: &SemanticTree, dir: FocusDirection, root_id: &str) {
        self.revalidate(tree);
        let mut ring: Vec<&Node> = tree
            .subtree(root_id)
            .into_iter()
            .filter(|n| n.is_focusable())
            .collect();
        // Containment wins over the within-mount restriction: never traverse
        // out of the active modal boundary.
        if let Some(boundary) = self.boundary.as_deref() {
            let scope: Vec<&str> = tree.subtree(boundary).iter().map(|n| n.id.as_str()).collect();
            ring.retain(|n| scope.contains(&n.id.as_str()));
            if ring.is_empty() {
                ring = self.ring(tree);
            }
        }
        self.move_ring(dir, ring);
    }

    fn move_ring(&mut self, dir: FocusDirection, ring: Vec<&Node>) {
        match dir {
            FocusDirection::Left | FocusDirection::Right => {
                if let Some(node) = self.focused_node_ring(&ring)
                    && node.kind() == crate::NodeKind::Input
                {
                    let len = node.content().chars().count();
                    match dir {
                        FocusDirection::Left => self.caret = self.caret.saturating_sub(1),
                        FocusDirection::Right => self.caret = (self.caret + 1).min(len),
                        _ => unreachable!(),
                    }
                }
            }
            FocusDirection::Next | FocusDirection::Down => {
                if ring.is_empty() {
                    self.focused = None;
                    self.caret = 0;
                    return;
                }
                let idx = ring
                    .iter()
                    .position(|n| Some(n.id.as_str()) == self.focused.as_deref());
                let next = match idx {
                    Some(i) if i + 1 < ring.len() => i + 1,
                    _ => 0,
                };
                self.focused = Some(ring[next].id.clone());
                self.caret = 0;
            }
            FocusDirection::Prev | FocusDirection::Up => {
                if ring.is_empty() {
                    self.focused = None;
                    self.caret = 0;
                    return;
                }
                let idx = ring
                    .iter()
                    .position(|n| Some(n.id.as_str()) == self.focused.as_deref());
                let next = match idx {
                    Some(0) | None => ring.len() - 1,
                    Some(i) => i - 1,
                };
                self.focused = Some(ring[next].id.clone());
                self.caret = 0;
            }
        }
    }

    fn focused_node_ring<'a>(&self, ring: &[&'a Node]) -> Option<&'a Node> {
        self.focused
            .as_deref()
            .and_then(|id| ring.iter().find(|n| n.id == id).copied())
    }

    pub fn focused_node<'a>(&self, tree: &'a SemanticTree) -> Option<&'a Node> {
        self.focused.as_deref().and_then(|id| tree.node(id))
    }

    /// The caret the renderer draws for `node`: only the focused input node
    /// carries a caret, clamped to its content length.
    pub fn caret_for(&self, node: &Node) -> usize {
        if node.kind() == crate::NodeKind::Input
            && self.focused.as_deref() == Some(node.id.as_str())
        {
            return self.caret.min(node.content().chars().count());
        }
        0
    }
}

/// Kernel-reserved interaction results. These are consumed by the kernel and
/// never reach a module (R-27). `CancelRun`/`Repaint` are NOT reserved: they
/// are remappable via bindings (decision 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedAction {
    /// Suspend the UI to the shell (Ctrl-Z). Reserved so modules cannot
    /// rebind the process-level suspend escape.
    Suspend,
    /// Enter kernel safe mode (Ctrl-X Ctrl-S).
    SafeModeChord,
    /// Leave the active modal focus boundary (Escape). Reserved only while a
    /// modal boundary is active; otherwise Escape forwards to the module.
    ModalEscape,
}

/// Result of classifying one decoded input event against the kernel-reserved
/// interaction set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputClass {
    /// Kernel action taken; the event never reaches a module.
    Reserved(ReservedAction),
    /// Consumed silently by the kernel (e.g. the safe-mode chord prefix).
    Consumed,
    /// Not reserved; the event may reach a module.
    Forward,
}

/// Classifies decoded input against the kernel-reserved interaction set.
/// Stateful: the safe-mode chord is two keys (Ctrl-X then Ctrl-S); any other
/// key clears the pending chord.
#[derive(Debug, Clone, Default)]
pub struct KeyClassifier {
    safe_mode_pending: bool,
}

impl KeyClassifier {
    pub fn new() -> Self {
        KeyClassifier {
            safe_mode_pending: false,
        }
    }

    pub fn classify(&mut self, e: &crate::InputEvent, modal_active: bool) -> InputClass {
        match e {
            crate::InputEvent::CtrlZ => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::Suspend)
            }
            crate::InputEvent::CtrlX => {
                self.safe_mode_pending = true;
                InputClass::Consumed
            }
            crate::InputEvent::Char('s') if self.safe_mode_pending => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::SafeModeChord)
            }
            // Escape is kernel-owned only while a modal boundary is active;
            // otherwise it belongs to the module.
            crate::InputEvent::Escape if modal_active => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::ModalEscape)
            }
            _ => {
                self.safe_mode_pending = false;
                InputClass::Forward
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Node;

    fn tree() -> SemanticTree {
        SemanticTree::new(
            Node::stack("root")
                .child(Node::button("a", "a"))
                .child(Node::input("input", "hi"))
                .child(Node::button("b", "b")),
        )
    }

    #[test]
    fn focus_ring_and_caret() {
        let t = tree();
        let mut f = FocusModel::new();
        f.revalidate(&t);
        assert_eq!(f.focused.as_deref(), Some("a"));
        f.move_focus(&t, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("input"));
        f.move_focus(&t, FocusDirection::Right);
        assert_eq!(f.caret, 1);
        f.move_focus(&t, FocusDirection::Right);
        assert_eq!(f.caret, 2);
        f.move_focus(&t, FocusDirection::Right);
        assert_eq!(f.caret, 2);
        f.move_focus(&t, FocusDirection::Left);
        assert_eq!(f.caret, 1);
        f.move_focus(&t, FocusDirection::Left);
        assert_eq!(f.caret, 0);
        f.move_focus(&t, FocusDirection::Left);
        assert_eq!(f.caret, 0);
        f.move_focus(&t, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("b"));
        // caret only renders on the focused input
        assert_eq!(f.caret_for(t.node("input").unwrap()), 0);
        f.move_focus(&t, FocusDirection::Prev);
        assert_eq!(f.focused.as_deref(), Some("input"));
    }

    #[test]
    fn focus_clamps_when_node_disappears() {
        let mut f = FocusModel::new();
        f.focused = Some("gone".into());
        f.revalidate(&tree());
        assert_eq!(f.focused.as_deref(), Some("a"));
    }

    #[test]
    fn reserved_keys() {
        let mut c = KeyClassifier::new();
        // CancelRun/Repaint are remappable now: Ctrl-C/Ctrl-L forward.
        assert_eq!(c.classify(&crate::InputEvent::CtrlC, false), InputClass::Forward);
        assert_eq!(c.classify(&crate::InputEvent::CtrlL, false), InputClass::Forward);
        // Suspend is reserved and wins.
        assert_eq!(c.classify(&crate::InputEvent::CtrlZ, false), InputClass::Reserved(ReservedAction::Suspend));
        assert_eq!(c.classify(&crate::InputEvent::CtrlX, false), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('x'), false), InputClass::Forward);
        assert_eq!(c.classify(&crate::InputEvent::CtrlX, false), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('s'), false), InputClass::Reserved(ReservedAction::SafeModeChord));
        // any other key clears the pending chord
        assert_eq!(c.classify(&crate::InputEvent::CtrlX, false), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('a'), false), InputClass::Forward);
        assert_eq!(c.classify(&crate::InputEvent::Char('s'), false), InputClass::Forward);
    }

    #[test]
    fn within_mount_ring_restricts_arrows() {
        let a = SemanticTree::new(
            Node::stack("root")
                .child(Node::input("input", "x"))
                .child(Node::button("btn", "btn")),
        );
        let b = SemanticTree::new(
            Node::stack("root").child(Node::input("input", "y")),
        );
        let composite = SemanticTree::compose(&[("main", &a), ("status", &b)]);
        let mut f = FocusModel::new();
        f.revalidate(&composite);
        assert_eq!(f.focused.as_deref(), Some("0.input"));
        // Down stays within mount 0: 0.input -> 0.btn (never 1.input)
        f.move_focus_within(&composite, FocusDirection::Down, "0.root");
        assert_eq!(f.focused.as_deref(), Some("0.btn"));
        f.move_focus_within(&composite, FocusDirection::Down, "0.root");
        assert_eq!(f.focused.as_deref(), Some("0.input"), "wraps within the mount");
        // Tab (full ring) crosses into mount 1 (0.input -> 0.btn -> 1.input)
        f.move_focus(&composite, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("0.btn"));
        f.move_focus(&composite, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("1.input"));
        // Up from mount 1 stays there
        f.move_focus_within(&composite, FocusDirection::Up, "1.root");
        assert_eq!(f.focused.as_deref(), Some("1.input"));
    }

    /// A tree with an outside button, a non-modal overlay button, and a
    /// topmost modal layer holding an input + button.
    fn modal_tree() -> SemanticTree {
        SemanticTree::new(
            Node::stack("root")
                .child(Node::button("outside", "outside"))
                .child(Node::layer("overlay", 1, false).child(Node::button("under", "under")))
                .child(
                    Node::layer("modal", 2, true)
                        .child(Node::input("m_input", "hi"))
                        .child(Node::button("m_btn", "ok")),
                ),
        )
    }

    #[test]
    fn modal_confines_focus_to_topmost_layer() {
        let t = modal_tree();
        let mut f = FocusModel::new();
        f.revalidate(&t);
        // entering the boundary moves focus to its first focusable
        assert_eq!(f.focused.as_deref(), Some("m_input"));
        assert_eq!(f.boundary(), Some("modal"));
        // traversal cycles only within the boundary
        f.move_focus(&t, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("m_btn"));
        f.move_focus(&t, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("m_input"));
        f.move_focus(&t, FocusDirection::Prev);
        assert_eq!(f.focused.as_deref(), Some("m_btn"));
        // within-mount traversal (Tab/arrows) cannot escape either
        f.move_focus_within(&t, FocusDirection::Down, "root");
        assert_eq!(f.focused.as_deref(), Some("m_input"));
        // the outside/non-modal focusables exist but stay unreachable
        assert!(t.is_focusable("outside"));
        assert!(t.is_focusable("under"));
    }

    #[test]
    fn focus_clamps_into_boundary_when_its_node_vanishes() {
        let mut f = FocusModel::new();
        f.focused = Some("outside".into());
        f.revalidate(&modal_tree());
        // focus is inside the boundary; a stale outside id cannot linger
        assert_eq!(f.focused.as_deref(), Some("m_input"));
    }

    #[test]
    fn modal_escape_restores_focus_and_frees_ring() {
        let t = modal_tree();
        let mut f = FocusModel::new();
        f.focused = Some("outside".into());
        f.revalidate(&t);
        assert_eq!(f.focused.as_deref(), Some("m_input"));
        f.escape_modal(&t);
        f.revalidate(&t);
        assert_eq!(f.boundary(), None);
        assert_eq!(
            f.focused.as_deref(),
            Some("outside"),
            "escape restores the pre-modal focus"
        );
        // the whole tree is reachable again
        f.move_focus(&t, FocusDirection::Next);
        assert_eq!(f.focused.as_deref(), Some("under"));
    }

    #[test]
    fn escape_is_reserved_only_under_an_active_modal() {
        let mut c = KeyClassifier::new();
        assert_eq!(
            c.classify(&crate::InputEvent::Escape, false),
            InputClass::Forward
        );
        assert_eq!(
            c.classify(&crate::InputEvent::Escape, true),
            InputClass::Reserved(ReservedAction::ModalEscape)
        );
    }
}
