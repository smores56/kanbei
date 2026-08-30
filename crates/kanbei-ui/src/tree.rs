//! The semantic tree: the module-facing UI model (R-27). Modules produce
//! `SemanticTree` data; the kernel renders it into cells. Hot paths consume
//! immutable Rust snapshots of this tree (consistency 13).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum nesting depth of a module-authored tree (kernel bound).
pub const MAX_TREE_DEPTH: usize = 32;
/// Maximum node count of a module-authored tree (kernel bound).
pub const MAX_TREE_NODES: usize = 4096;

/// Node kinds understood by the kernel renderer. Unknown kinds are rejected
/// at parse time (fail-closed, R-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    Header,
    Status,
    List,
    ListItem,
    Text,
    Input,
    Button,
    Placeholder,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Root => "root",
            NodeKind::Header => "header",
            NodeKind::Status => "status",
            NodeKind::List => "list",
            NodeKind::ListItem => "list_item",
            NodeKind::Text => "text",
            NodeKind::Input => "input",
            NodeKind::Button => "button",
            NodeKind::Placeholder => "placeholder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "root" => NodeKind::Root,
            "header" => NodeKind::Header,
            "status" => NodeKind::Status,
            "list" => NodeKind::List,
            "list_item" => NodeKind::ListItem,
            "text" => NodeKind::Text,
            "input" => NodeKind::Input,
            "button" => NodeKind::Button,
            "placeholder" => NodeKind::Placeholder,
            _ => return None,
        })
    }
}

/// One semantic node. `id` is the module-stable identity the kernel's focus
/// model references across renders; the kernel clamps focus when an id
/// disappears (focus/modal invariants, R-27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub content: String,
    /// Named style key into the active theme (kernel resolves unknown keys
    /// to the default style).
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub focusable: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(id: impl Into<String>, kind: NodeKind) -> Self {
        Node {
            id: id.into(),
            kind,
            content: String::new(),
            style: None,
            focusable: false,
            disabled: false,
            children: Vec::new(),
        }
    }

    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    pub fn with_style(mut self, style: impl Into<String>) -> Self {
        self.style = Some(style.into());
        self
    }

    pub fn focusable(mut self) -> Self {
        self.focusable = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn child(mut self, node: Node) -> Self {
        self.children.push(node);
        self
    }
}

/// An immutable module-authored UI snapshot. `root.kind` must be `Root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTree {
    pub root: Node,
}

/// Parse failures are kernel faults (fail-closed): an unparseable tree never
/// reaches the renderer.
#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("tree payload must be an object with a \"root\" node")]
    NotAnObject,
    #[error("root node kind must be \"root\", got {0:?}")]
    BadRootKind(String),
    #[error("node {id:?} has unknown kind {kind:?}")]
    UnknownKind { id: String, kind: String },
    #[error("node {id:?} exceeds the maximum tree depth {MAX_TREE_DEPTH}")]
    TooDeep { id: String },
    #[error("tree exceeds the maximum node count {MAX_TREE_NODES}")]
    TooManyNodes,
}

impl SemanticTree {
    pub fn new(root: Node) -> Self {
        SemanticTree { root }
    }

    /// Parse the module wire shape `{"root": {...}}`. Rejects unknown kinds,
    /// non-root roots, and oversized trees (kernel bounds).
    pub fn from_json(v: &Value) -> Result<Self, TreeError> {
        let obj = v.as_object().ok_or(TreeError::NotAnObject)?;
        let root_value = obj.get("root").ok_or(TreeError::NotAnObject)?;
        let mut count = 0;
        let root = Self::parse_node(root_value, 0, &mut count)?;
        if root.kind != NodeKind::Root {
            return Err(TreeError::BadRootKind(root.kind.as_str().to_string()));
        }
        Ok(SemanticTree { root })
    }

    fn parse_node(v: &Value, depth: usize, count: &mut usize) -> Result<Node, TreeError> {
        if depth > MAX_TREE_DEPTH {
            return Err(TreeError::TooDeep {
                id: v.get("id").and_then(Value::as_str).unwrap_or("?").to_string(),
            });
        }
        *count += 1;
        if *count > MAX_TREE_NODES {
            return Err(TreeError::TooManyNodes);
        }
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let kind_str = v
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| TreeError::UnknownKind {
                id: id.clone(),
                kind: v.get("kind").map(Value::to_string).unwrap_or_default(),
            })?;
        let kind = NodeKind::parse(kind_str).ok_or_else(|| TreeError::UnknownKind {
            id: id.clone(),
            kind: kind_str.to_string(),
        })?;
        let mut node = Node {
            id,
            kind,
            content: v.get("content").and_then(Value::as_str).unwrap_or_default().to_string(),
            style: v.get("style").and_then(Value::as_str).map(String::from),
            focusable: v.get("focusable").and_then(Value::as_bool).unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            children: Vec::new(),
        };
        if let Some(children) = v.get("children").and_then(Value::as_array) {
            for child in children {
                node.children.push(Self::parse_node(child, depth + 1, count)?);
            }
        }
        Ok(node)
    }

    /// Serialize to the module wire shape `{"root": {...}}`.
    pub fn to_json(&self) -> Value {
        serde_json::json!({ "root": self.root })
    }

    /// All nodes in depth-first preorder.
    pub fn nodes(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        fn walk<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(n);
            for c in &n.children {
                walk(c, out);
            }
        }
        walk(&self.root, &mut out);
        out
    }

    /// Focusable, non-disabled nodes in depth-first order (the kernel's focus
    /// ring).
    pub fn focusable(&self) -> Vec<&Node> {
        self.nodes()
            .into_iter()
            .filter(|n| n.focusable && !n.disabled)
            .collect()
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes().into_iter().find(|n| n.id == id)
    }

    pub fn is_focusable(&self, id: &str) -> bool {
        self.focusable().iter().any(|n| n.id == id)
    }

    /// The node the input line renders for: the focused input, else the first
    /// input, else `None` (the kernel shows an empty prompt).
    pub fn input_node(&self, focused: Option<&str>) -> Option<&Node> {
        if let Some(id) = focused
            && let Some(n) = self.node(id)
            && n.kind == NodeKind::Input
            && !n.disabled
        {
            return Some(n);
        }
        self.nodes()
            .into_iter()
            .find(|n| n.kind == NodeKind::Input && !n.disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_json() {
        let tree = SemanticTree::new(
            Node::new("root", NodeKind::Root).child(
                Node::new("input", NodeKind::Input)
                    .with_content("hi")
                    .focusable(),
            ),
        );
        let v = tree.to_json();
        let parsed = SemanticTree::from_json(&v).unwrap();
        assert_eq!(parsed, tree);
        assert!(parsed.is_focusable("input"));
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = SemanticTree::from_json(&json!({"root": {"id": "r", "kind": "carousel"}}))
            .unwrap_err();
        assert!(matches!(err, TreeError::UnknownKind { .. }));
    }

    #[test]
    fn rejects_bad_root_kind() {
        let err = SemanticTree::from_json(&json!({"root": {"id": "r", "kind": "list"}}))
            .unwrap_err();
        assert!(matches!(err, TreeError::BadRootKind(_)));
    }

    #[test]
    fn rejects_oversized_tree() {
        let mut node = json!({"id": "leaf", "kind": "text"});
        for _ in 0..MAX_TREE_DEPTH + 1 {
            node = json!({"id": "n", "kind": "list", "children": [node]});
        }
        let err = SemanticTree::from_json(&json!({"root": node})).unwrap_err();
        assert!(matches!(err, TreeError::TooDeep { .. }));
    }

    #[test]
    fn focusable_ring_order() {
        let tree = SemanticTree::new(
            Node::new("root", NodeKind::Root).child(
                Node::new("list", NodeKind::List)
                    .child(Node::new("a", NodeKind::Button).focusable())
                    .child(Node::new("b", NodeKind::Button).focusable().disabled())
                    .child(Node::new("c", NodeKind::Input).focusable()),
            ),
        );
        let ring: Vec<&str> = tree.focusable().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ring, vec!["a", "c"]);
        assert!(!tree.is_focusable("b"));
        assert_eq!(tree.input_node(Some("a")).unwrap().id, "c");
    }
}
