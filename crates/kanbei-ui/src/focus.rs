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
        }
    }

    /// Restore the invariants against the current tree: focus names a
    /// focusable, non-disabled node; caret is clamped to the focused input's
    /// content length; viewport_top is non-negative.
    pub fn revalidate(&mut self, tree: &SemanticTree) {
        match &self.focused {
            Some(id) if tree.is_focusable(id) => {}
            _ => {
                self.focused = tree.focusable().first().map(|n| n.id.clone());
                self.caret = 0;
            }
        }
        if let Some(node) = self.focused_node(tree) {
            if node.kind == crate::NodeKind::Input {
                self.caret = self.caret.min(node.content.chars().count());
            } else {
                self.caret = 0;
            }
        }
        self.viewport_top = self.viewport_top.max(0);
    }

    /// Move focus through the focusable ring. Left/Right move the caret when
    /// the focused node is an input and are otherwise no-ops.
    pub fn move_focus(&mut self, tree: &SemanticTree, dir: FocusDirection) {
        self.revalidate(tree);
        match dir {
            FocusDirection::Left | FocusDirection::Right => {
                if let Some(node) = self.focused_node(tree) {
                    if node.kind == crate::NodeKind::Input {
                        let len = node.content.chars().count();
                        match dir {
                            FocusDirection::Left => self.caret = self.caret.saturating_sub(1),
                            FocusDirection::Right => self.caret = (self.caret + 1).min(len),
                            _ => unreachable!(),
                        }
                    }
                }
            }
            FocusDirection::Next | FocusDirection::Down => {
                let ring = tree.focusable();
                if ring.is_empty() {
                    self.focused = None;
                    self.caret = 0;
                    return;
                }
                let idx = ring.iter().position(|n| Some(n.id.as_str()) == self.focused.as_deref());
                let next = match idx {
                    Some(i) if i + 1 < ring.len() => i + 1,
                    _ => 0,
                };
                self.focused = Some(ring[next].id.clone());
                self.caret = 0;
            }
            FocusDirection::Prev | FocusDirection::Up => {
                let ring = tree.focusable();
                if ring.is_empty() {
                    self.focused = None;
                    self.caret = 0;
                    return;
                }
                let idx = ring.iter().position(|n| Some(n.id.as_str()) == self.focused.as_deref());
                let next = match idx {
                    Some(0) | None => ring.len() - 1,
                    Some(i) => i - 1,
                };
                self.focused = Some(ring[next].id.clone());
                self.caret = 0;
            }
        }
    }

    pub fn focused_node<'a>(&self, tree: &'a SemanticTree) -> Option<&'a Node> {
        self.focused.as_deref().and_then(|id| tree.node(id))
    }

    /// The caret the renderer draws for `node`: only the focused input node
    /// carries a caret, clamped to its content length.
    pub fn caret_for(&self, node: &Node) -> usize {
        if node.kind == crate::NodeKind::Input
            && self.focused.as_deref() == Some(node.id.as_str())
        {
            return self.caret.min(node.content.chars().count());
        }
        0
    }
}

/// Kernel-reserved interaction results. These are consumed by the kernel and
/// never reach a module (R-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedAction {
    /// Modal escape / cancel the active run (Ctrl-C).
    CancelRun,
    /// Repaint the whole frame (Ctrl-L).
    Repaint,
    /// Enter kernel safe mode (Ctrl-X Ctrl-S).
    SafeModeChord,
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

    pub fn classify(&mut self, e: &crate::InputEvent) -> InputClass {
        match e {
            crate::InputEvent::CtrlC => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::CancelRun)
            }
            crate::InputEvent::CtrlL => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::Repaint)
            }
            crate::InputEvent::CtrlX => {
                self.safe_mode_pending = true;
                InputClass::Consumed
            }
            crate::InputEvent::Char('s') if self.safe_mode_pending => {
                self.safe_mode_pending = false;
                InputClass::Reserved(ReservedAction::SafeModeChord)
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
    use crate::{Node, NodeKind};

    fn tree() -> SemanticTree {
        SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("a", NodeKind::Button).focusable())
                .child(Node::new("input", NodeKind::Input).with_content("hi").focusable())
                .child(Node::new("b", NodeKind::Button).focusable()),
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
        assert_eq!(f.caret_for(&t.node("input").unwrap()), 0);
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
        assert_eq!(c.classify(&crate::InputEvent::CtrlC), InputClass::Reserved(ReservedAction::CancelRun));
        assert_eq!(c.classify(&crate::InputEvent::CtrlL), InputClass::Reserved(ReservedAction::Repaint));
        assert_eq!(c.classify(&crate::InputEvent::CtrlX), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('x')), InputClass::Forward);
        assert_eq!(c.classify(&crate::InputEvent::CtrlX), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('s')), InputClass::Reserved(ReservedAction::SafeModeChord));
        // any other key clears the pending chord
        assert_eq!(c.classify(&crate::InputEvent::CtrlX), InputClass::Consumed);
        assert_eq!(c.classify(&crate::InputEvent::Char('a')), InputClass::Forward);
        assert_eq!(c.classify(&crate::InputEvent::Char('s')), InputClass::Forward);
    }
}
