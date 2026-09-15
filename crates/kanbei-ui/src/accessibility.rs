//! Kernel-owned accessibility validation (R-27). Every module-authored tree
//! passes this pass before rendering; structural violations are kernel
//! faults (placeholder + degraded), softer issues are surfaced as warnings.

use crate::tree::{NodeKind, SemanticTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub node_id: String,
    pub severity: Severity,
    pub message: String,
}

impl Issue {
    fn error(node_id: &str, message: impl Into<String>) -> Self {
        Issue {
            node_id: node_id.to_string(),
            severity: Severity::Error,
            message: message.into(),
        }
    }

    fn warning(node_id: &str, message: impl Into<String>) -> Self {
        Issue {
            node_id: node_id.to_string(),
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// Validate a tree. Errors are structural (the kernel renders a placeholder
/// and marks the module degraded); warnings are advisory.
pub fn validate(tree: &SemanticTree) -> Vec<Issue> {
    let mut issues = Vec::new();
    walk(&tree.root, false, &mut issues);
    issues
}

fn walk(node: &crate::Node, disabled_ancestor: bool, issues: &mut Vec<Issue>) {
    let disabled = disabled_ancestor || node.disabled;
    if node.id.is_empty() {
        issues.push(Issue::error(&node.id, "node has an empty id"));
    }
    // Focusability is intrinsic to the interactive kinds; an input may be
    // empty (an empty prompt), the others need a label to be interactive.
    if node.is_interactive() && node.kind() != NodeKind::Input && node.label().is_empty() {
        issues.push(Issue::error(&node.id, "focusable node has no label/content"));
    }
    if node.is_interactive() && disabled {
        issues.push(Issue::error(&node.id, "focusable node is inside a disabled subtree"));
    }
    // A modal layer with no focusable descendant is unusable: the kernel
    // cannot place focus inside it (inescapable modal).
    if node.modal() && !node.has_focusable_descendant() {
        issues.push(Issue::error(
            &node.id,
            "modal layer has no focusable descendant",
        ));
    }
    if node.label().chars().any(|c| c.is_control()) {
        issues.push(Issue::warning(&node.id, "content contains control characters"));
    }
    for child in &node.children {
        walk(child, disabled, issues);
    }
}

/// Whether a tree is structurally usable: no error-severity issues and a
/// root present (the parser guarantees the root).
pub fn is_valid(tree: &SemanticTree) -> bool {
    validate(tree).iter().all(|i| i.severity != Severity::Error)
}

pub fn focusable_node_has_label(tree: &SemanticTree, id: &str) -> bool {
    tree.node(id)
        .map(|n| n.is_focusable() && !n.label().is_empty())
        .unwrap_or(false)
}

/// The error issues for one node id (test helper).
pub fn issues_for(tree: &SemanticTree, id: &str) -> Vec<Issue> {
    validate(tree)
        .into_iter()
        .filter(|i| i.node_id == id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ListItem, Node};

    #[test]
    fn valid_tree_has_no_errors() {
        let t = SemanticTree::new(
            Node::stack("root").child(Node::button("a", "go")),
        );
        assert!(is_valid(&t));
        assert!(validate(&t).is_empty());
    }

    #[test]
    fn selectable_list_without_label_is_error() {
        let t = SemanticTree::new(Node::stack("root").child(Node::list(
            "l",
            vec![ListItem::new("i", "").selectable()],
        )));
        assert!(!is_valid(&t));
        let issues = issues_for(&t, "l");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Error);
    }

    #[test]
    fn empty_button_is_not_focusable() {
        let t = SemanticTree::new(Node::stack("root").child(Node::button("a", "")));
        // the label requirement is part of the focusable predicate
        assert!(t.focusable().is_empty());
        assert!(is_valid(&t));
    }

    #[test]
    fn disabled_subtree_focusable_is_error() {
        let t = SemanticTree::new(
            Node::stack("root").child(
                Node::col("col")
                    .disabled()
                    .child(Node::button("a", "x")),
            ),
        );
        let issues = issues_for(&t, "a");
        assert!(issues.iter().any(|i| i.severity == Severity::Error));
    }

    #[test]
    fn control_chars_are_warnings() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text("t", "a\tb")));
        let issues = issues_for(&t, "t");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
    }

    #[test]
    fn empty_ids_reported() {
        let t = SemanticTree::new(
            Node::stack("root").child(Node::text("", "x")),
        );
        assert!(validate(&t).iter().any(|i| i.node_id.is_empty()));
    }

    #[test]
    fn modal_without_focusable_descendant_is_error() {
        let t = SemanticTree::new(
            Node::stack("root")
                .child(Node::layer("modal", 1, true).child(Node::text("t", "just text"))),
        );
        assert!(!is_valid(&t));
        let issues = issues_for(&t, "modal");
        assert!(issues.iter().any(|i| i.severity == Severity::Error));
        // a modal with a focusable descendant is usable
        let ok = SemanticTree::new(
            Node::stack("root")
                .child(Node::layer("modal", 1, true).child(Node::input("i", ""))),
        );
        assert!(is_valid(&ok));
        // focusables OUTSIDE the boundary are unreachable, not an error
        let reachable = SemanticTree::new(
            Node::stack("root")
                .child(Node::button("outside", "outside"))
                .child(Node::layer("modal", 1, true).child(Node::input("i", ""))),
        );
        assert!(is_valid(&reachable));
    }
}
