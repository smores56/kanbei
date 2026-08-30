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
    if node.focusable && node.content.is_empty() && node.kind != NodeKind::Input {
        issues.push(Issue::error(&node.id, "focusable node has no label/content"));
    }
    if node.focusable && disabled {
        issues.push(Issue::error(&node.id, "focusable node is inside a disabled subtree"));
    }
    if node.content.chars().any(|c| c.is_control()) {
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
        .map(|n| n.focusable && !n.content.is_empty())
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
    use crate::{Node, NodeKind};

    #[test]
    fn valid_tree_has_no_errors() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("a", NodeKind::Button).with_content("go").focusable()),
        );
        assert!(is_valid(&t));
        assert!(validate(&t).is_empty());
    }

    #[test]
    fn focusable_without_label_is_error() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("a", NodeKind::Button).focusable()),
        );
        assert!(!is_valid(&t));
        let issues = issues_for(&t, "a");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Error);
    }

    #[test]
    fn disabled_subtree_focusable_is_error() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root).child(
                Node::new("list", NodeKind::List)
                    .disabled()
                    .child(Node::new("a", NodeKind::Button).with_content("x").focusable()),
            ),
        );
        let issues = issues_for(&t, "a");
        assert!(issues.iter().any(|i| i.severity == Severity::Error));
    }

    #[test]
    fn control_chars_are_warnings() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("t", NodeKind::Text).with_content("a\tb")),
        );
        let issues = issues_for(&t, "t");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
    }

    #[test]
    fn empty_ids_reported() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root).child(Node::new("", NodeKind::Text).with_content("x")),
        );
        assert!(validate(&t).iter().any(|i| i.node_id.is_empty()));
    }
}
