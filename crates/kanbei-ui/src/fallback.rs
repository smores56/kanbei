//! Kernel fallback surface (R-27 fault classes). The kernel never depends on
//! a module for a usable UI:
//! - composition-validation failure → last-valid UI + staleness banner
//!   (overlaid at render time, see [`staleness_text`]);
//! - runtime component fault → kernel-authored placeholder tree
//!   ([`placeholder_tree`]) with the module marked degraded by the caller;
//! - kernel render fault → the kernel fallback UI ([`FallbackUi`]).
//!
//! All three are pure Rust and module-free (consistency 13).

use crate::tree::{Node, SemanticTree};

pub const STALE_PREFIX: &str = "composition stale";

/// The staleness banner text overlaid on the last-valid UI after a failed
/// composition publish.
pub fn staleness_text(reason: &str) -> String {
    format!("{STALE_PREFIX}: {reason}")
}

/// The kernel-authored placeholder shown for a runtime component fault
/// (R-27 fault class 2). The caller marks the module degraded alongside.
pub fn placeholder_tree(component: &str, error: &str) -> SemanticTree {
    SemanticTree::new(
        Node::stack("root")
            .child(Node::styled_text(
                "header",
                format!("kanbei — {component}"),
                "header",
            ))
            .child(Node::styled_text(
                "fault",
                format!("UI component faulted: {error}"),
                "error",
            ))
            .child(Node::text("hint", "Ctrl-X Ctrl-S enters safe mode")),
    )
}

/// The kernel fallback UI (R-27 fault class 3 / safe mode). Rendered without
/// any module call; input is not forwarded to modules in this state.
pub struct FallbackUi {
    pub message: String,
}

impl FallbackUi {
    pub fn new(message: impl Into<String>) -> Self {
        FallbackUi {
            message: message.into(),
        }
    }

    /// The fallback semantic tree (kernel-authored, module-free).
    pub fn tree(&self) -> SemanticTree {
        SemanticTree::new(
            Node::stack("root")
                .child(Node::styled_text("header", "kanbei safe mode", "header"))
                .child(Node::text("msg", &self.message))
                .child(Node::text(
                    "hint",
                    "Kernel fallback UI: modules are not rendering. Restart to restore the workbench.",
                ))
                .child(Node::input("input", "")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_prefix() {
        assert_eq!(staleness_text("publish failed"), "composition stale: publish failed");
    }

    #[test]
    fn placeholder_is_kernel_tree() {
        let t = placeholder_tree("workbench", "reduce failed");
        assert_eq!(t.root.kind(), crate::NodeKind::Stack);
        assert!(t.nodes().iter().any(|n| n.content().contains("UI component faulted")));
        assert!(crate::accessibility::is_valid(&t));
    }

    #[test]
    fn fallback_tree_has_input() {
        let fb = FallbackUi::new("boom");
        let t = fb.tree();
        assert!(t.input_node(None).is_some());
        assert!(crate::accessibility::is_valid(&t));
    }
}
