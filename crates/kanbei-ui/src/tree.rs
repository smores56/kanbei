//! The semantic tree: the module-facing UI model (R-27). Modules produce
//! `SemanticTree` data; the kernel renders it into cells. Hot paths consume
//! immutable Rust snapshots of this tree (consistency 13).
//!
//! The tree is built from minimal typed primitives, not semantic widgets:
//! layout (`stack`/`row`/`col`), content (`text`/`code`), interactive
//! (`input`/`list`/`button`), and `layer`. A module composes the rows it
//! wants (a response bubble is a `stack` of styled `text` spans) instead of
//! asking the kernel for named semantic rows.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Maximum nesting depth of a module-authored tree (kernel bound).
pub const MAX_TREE_DEPTH: usize = 32;
/// Maximum node count of a module-authored tree (kernel bound).
pub const MAX_TREE_NODES: usize = 4096;

/// The primitive node kinds understood by the kernel renderer. Unknown kinds
/// are rejected at parse time (fail-closed, R-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Vertical layout container; `z` orders it against its siblings.
    Stack,
    /// Horizontal layout container.
    Row,
    /// Vertical layout container (no sibling ordering).
    Col,
    /// Styled text content (`spans`).
    Text,
    /// Monospace/tool content (`spans`).
    Code,
    /// Editable text (`content` is the draft).
    Input,
    /// Selectable item list (`items`).
    List,
    /// Activatable control (`label`).
    Button,
    /// Overlay container; `z` orders siblings, `modal` marks focus
    /// containment (containment is the kernel's, not the module's).
    Layer,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Stack => "stack",
            NodeKind::Row => "row",
            NodeKind::Col => "col",
            NodeKind::Text => "text",
            NodeKind::Code => "code",
            NodeKind::Input => "input",
            NodeKind::List => "list",
            NodeKind::Button => "button",
            NodeKind::Layer => "layer",
        }
    }

    /// Parse a kind from its wire name. `None` for unknown kinds (fail-closed,
    /// R-27).
    pub fn parse(s: &str) -> Option<Self> {
        let k = match s {
            "stack" => NodeKind::Stack,
            "row" => NodeKind::Row,
            "col" => NodeKind::Col,
            "text" => NodeKind::Text,
            "code" => NodeKind::Code,
            "input" => NodeKind::Input,
            "list" => NodeKind::List,
            "button" => NodeKind::Button,
            "layer" => NodeKind::Layer,
            _ => return None,
        };
        Some(k)
    }
}

/// One styled text segment of a `text`/`code` node. `style` names a theme
/// key; unknown keys resolve to the default style (kernel resolves).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: Some(style.into()),
        }
    }
}

/// One item of a `list` node. `selectable` items make the list interactive
/// (and focusable); non-selectable items are display-only rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub selectable: bool,
}

impl ListItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        ListItem {
            id: id.into(),
            label: label.into(),
            selectable: false,
        }
    }

    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The per-kind payload of a node (the `<kind props>` of the wire shape).
/// Variants are the source of truth for a node's [`NodeKind`], so an illegal
/// kind/props pairing cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeProps {
    Stack { z: i32 },
    Row,
    Col,
    Text { spans: Vec<Span> },
    Code { spans: Vec<Span> },
    Input { content: String },
    List { items: Vec<ListItem> },
    Button { label: String },
    Layer { z: i32, modal: bool },
}

impl NodeProps {
    pub fn kind(&self) -> NodeKind {
        match self {
            NodeProps::Stack { .. } => NodeKind::Stack,
            NodeProps::Row => NodeKind::Row,
            NodeProps::Col => NodeKind::Col,
            NodeProps::Text { .. } => NodeKind::Text,
            NodeProps::Code { .. } => NodeKind::Code,
            NodeProps::Input { .. } => NodeKind::Input,
            NodeProps::List { .. } => NodeKind::List,
            NodeProps::Button { .. } => NodeKind::Button,
            NodeProps::Layer { .. } => NodeKind::Layer,
        }
    }
}

/// One primitive node. `id` is the module-stable identity the kernel's focus
/// model references across renders; the kernel clamps focus when an id
/// disappears (focus/modal invariants, R-27).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub props: NodeProps,
    pub disabled: bool,
    pub children: Vec<Node>,
}

impl Node {
    fn bare(id: impl Into<String>, props: NodeProps) -> Self {
        Node {
            id: id.into(),
            props,
            disabled: false,
            children: Vec::new(),
        }
    }

    pub fn stack(id: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Stack { z: 0 })
    }

    pub fn stack_z(id: impl Into<String>, z: i32) -> Self {
        Self::bare(id, NodeProps::Stack { z })
    }

    pub fn row(id: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Row)
    }

    pub fn col(id: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Col)
    }

    pub fn text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Text {
            spans: vec![Span::plain(text)],
        })
    }

    pub fn styled_text(
        id: impl Into<String>,
        text: impl Into<String>,
        style: impl Into<String>,
    ) -> Self {
        Self::bare(id, NodeProps::Text {
            spans: vec![Span::styled(text, style)],
        })
    }

    pub fn text_spans(id: impl Into<String>, spans: Vec<Span>) -> Self {
        Self::bare(id, NodeProps::Text { spans })
    }

    pub fn code(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Code {
            spans: vec![Span::plain(text)],
        })
    }

    pub fn code_spans(id: impl Into<String>, spans: Vec<Span>) -> Self {
        Self::bare(id, NodeProps::Code { spans })
    }

    pub fn input(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Input {
            content: content.into(),
        })
    }

    pub fn list(id: impl Into<String>, items: Vec<ListItem>) -> Self {
        Self::bare(id, NodeProps::List { items })
    }

    pub fn button(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::bare(id, NodeProps::Button {
            label: label.into(),
        })
    }

    pub fn layer(id: impl Into<String>, z: i32, modal: bool) -> Self {
        Self::bare(id, NodeProps::Layer { z, modal })
    }

    pub fn child(mut self, node: Node) -> Self {
        self.children.push(node);
        self
    }

    pub fn children(mut self, nodes: Vec<Node>) -> Self {
        self.children = nodes;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn kind(&self) -> NodeKind {
        self.props.kind()
    }

    /// Sibling paint order (only `stack`/`layer` carry it; every other kind
    /// defaults to 0).
    pub fn z(&self) -> i32 {
        match &self.props {
            NodeProps::Stack { z } | NodeProps::Layer { z, .. } => *z,
            _ => 0,
        }
    }

    pub fn modal(&self) -> bool {
        matches!(self.props, NodeProps::Layer { modal: true, .. })
    }

    pub fn spans(&self) -> &[Span] {
        match &self.props {
            NodeProps::Text { spans } | NodeProps::Code { spans } => spans,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[ListItem] {
        match &self.props {
            NodeProps::List { items } => items,
            _ => &[],
        }
    }

    /// The node's primary string: the input draft, the button label, or the
    /// concatenated text spans.
    pub fn content(&self) -> String {
        match &self.props {
            NodeProps::Input { content } => content.clone(),
            NodeProps::Button { label } => label.clone(),
            NodeProps::Text { spans } | NodeProps::Code { spans } => {
                spans.iter().map(|s| s.text.as_str()).collect()
            }
            _ => String::new(),
        }
    }

    /// The accessibility label: for a list, its items' labels are the label
    /// source (an item list is only interactive when it has selectable items).
    pub fn label(&self) -> String {
        match &self.props {
            NodeProps::List { items } => items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            _ => self.content(),
        }
    }

    /// Whether the kind is inherently interactive (ignores `disabled`, which
    /// the accessibility pass checks separately). A `button` needs a label; a
    /// `list` needs a selectable item; an `input` is always interactive.
    pub fn is_interactive(&self) -> bool {
        match &self.props {
            NodeProps::Input { .. } => true,
            NodeProps::Button { label } => !label.is_empty(),
            NodeProps::List { items } => items.iter().any(|i| i.selectable),
            _ => false,
        }
    }

    /// Ring membership: interactive and not disabled.
    pub fn is_focusable(&self) -> bool {
        self.is_interactive() && !self.disabled
    }
}

/// An immutable module-authored UI snapshot. `root.kind` must be `Stack`.
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
    #[error("root node kind must be \"stack\", got {0:?}")]
    BadRootKind(String),
    #[error("node {id:?} has unknown kind {kind:?}")]
    UnknownKind { id: String, kind: String },
    #[error("node {id:?} has malformed {prop} property: {detail}")]
    BadProps {
        id: String,
        prop: &'static str,
        detail: String,
    },
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
    /// malformed kind props, non-`stack` roots, and oversized trees (kernel
    /// bounds).
    pub fn from_json(v: &Value) -> Result<Self, TreeError> {
        let obj = v.as_object().ok_or(TreeError::NotAnObject)?;
        let root_value = obj.get("root").ok_or(TreeError::NotAnObject)?;
        let mut count = 0;
        let root = Self::parse_node(root_value, 0, &mut count)?;
        if root.kind() != NodeKind::Stack {
            return Err(TreeError::BadRootKind(root.kind().as_str().to_string()));
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
        let props = Self::parse_props(kind, v, &id)?;
        let disabled = match v.get("disabled") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(bad_props(&id, "disabled", "expected a boolean")),
        };
        let mut node = Node {
            id,
            props,
            disabled,
            children: Vec::new(),
        };
        if let Some(children) = v.get("children") {
            let children = lua_array(children)
                .ok_or_else(|| bad_props(&node.id, "children", "expected an array"))?;
            for child in children {
                node.children.push(Self::parse_node(child, depth + 1, count)?);
            }
        }
        Ok(node)
    }

    fn parse_props(kind: NodeKind, v: &Value, id: &str) -> Result<NodeProps, TreeError> {
        Ok(match kind {
            NodeKind::Stack => NodeProps::Stack {
                z: parse_i32(v, "z", id)?,
            },
            NodeKind::Row => NodeProps::Row,
            NodeKind::Col => NodeProps::Col,
            NodeKind::Text => NodeProps::Text {
                spans: parse_spans(v, id)?,
            },
            NodeKind::Code => NodeProps::Code {
                spans: parse_spans(v, id)?,
            },
            NodeKind::Input => NodeProps::Input {
                content: parse_string(v, "content", id)?.unwrap_or_default(),
            },
            NodeKind::List => NodeProps::List {
                items: parse_items(v, id)?,
            },
            NodeKind::Button => NodeProps::Button {
                label: parse_string(v, "label", id)?.unwrap_or_default(),
            },
            NodeKind::Layer => NodeProps::Layer {
                z: parse_i32(v, "z", id)?,
                modal: match v.get("modal") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(bad_props(id, "modal", "expected a boolean")),
                },
            },
        })
    }

    /// Serialize to the module wire shape `{"root": {...}}`.
    pub fn to_json(&self) -> Value {
        Value::Object(Map::from_iter([(
            "root".to_string(),
            node_to_value(&self.root),
        )]))
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
        self.nodes().into_iter().filter(|n| n.is_focusable()).collect()
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
            && n.kind() == NodeKind::Input
            && !n.disabled
        {
            return Some(n);
        }
        self.nodes()
            .into_iter()
            .find(|n| n.kind() == NodeKind::Input && !n.disabled)
    }

    /// Nodes at or below `root_id` in depth-first order (the within-mount
    /// focus ring of the composite, M8). Empty when the id is unknown.
    pub fn subtree(&self, root_id: &str) -> Vec<&Node> {
        let mut out = Vec::new();
        fn walk<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(n);
            for c in &n.children {
                walk(c, out);
            }
        }
        if let Some(found) = self.nodes().into_iter().find(|n| n.id == root_id) {
            walk(found, &mut out);
        }
        out
    }

    /// Compose per-mount trees into one synthetic composite root (M8
    /// multi-module UI): each mount's own root node becomes a child of a new
    /// synthetic `stack` root, and every node id is prefixed with `"{index}."`
    /// so focus identity is unambiguous across mounts (two mounts may use the
    /// same ids). The synthetic root carries no content and is never
    /// focusable. The input order IS the slot order the caller determined;
    /// `index` is the mount's position in that order. A single mount is
    /// returned verbatim (no prefix, no wrapper) so the single-mount
    /// workbench stays byte-identical to M5.
    pub fn compose(mounts: &[(&str, &SemanticTree)]) -> Self {
        if let [(_slot, single)] = mounts {
            return (*single).clone();
        }
        let children: Vec<Node> = mounts
            .iter()
            .enumerate()
            .map(|(i, (_, tree))| prefix_node(&tree.root, i))
            .collect();
        SemanticTree::new(Node::stack("composite").children(children))
    }

    /// Resolve a composite id (see [`Self::compose`]) back to
    /// `(mount index, original id)`. The first `.` always separates the
    /// index because the prefix is prepended verbatim.
    pub fn split_composite_id(id: &str) -> Option<(usize, &str)> {
        let (index, rest) = id.split_once('.')?;
        Some((index.parse().ok()?, rest))
    }
}

fn bad_props(id: &str, prop: &'static str, detail: impl Into<String>) -> TreeError {
    TreeError::BadProps {
        id: id.to_string(),
        prop,
        detail: detail.into(),
    }
}

/// The guest JSON encoder renders an empty Lua table as `{}` (it cannot tell
/// an empty array from an empty object); accept that as an empty array so a
/// module's empty `items`/`spans`/`children` is not a fault.
fn lua_array(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Array(a) => Some(a),
        Value::Object(o) if o.is_empty() => Some(&[]),
        _ => None,
    }
}

fn parse_i32(v: &Value, key: &str, id: &str) -> Result<i32, TreeError> {
    match v.get(key) {
        None => Ok(0),
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|x| i32::try_from(x).ok())
            .ok_or_else(|| bad_props(id, "z", "expected a 32-bit integer")),
        Some(_) => Err(bad_props(id, "z", "expected a 32-bit integer")),
    }
}

fn parse_string(v: &Value, key: &str, id: &str) -> Result<Option<String>, TreeError> {
    match v.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad_props(id, "content", "expected a string")),
    }
}

fn parse_spans(v: &Value, id: &str) -> Result<Vec<Span>, TreeError> {
    let arr = v
        .get("spans")
        .and_then(lua_array)
        .ok_or_else(|| bad_props(id, "spans", "expected an array"))?;
    arr.iter()
        .map(|s| {
            let obj = s
                .as_object()
                .ok_or_else(|| bad_props(id, "spans", "each span must be an object"))?;
            let text = obj
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_props(id, "spans", "each span needs a string \"text\""))?;
            let style = match obj.get("style") {
                None => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(bad_props(id, "spans", "span \"style\" must be a string")),
            };
            Ok(Span {
                text: text.to_string(),
                style,
            })
        })
        .collect()
}

fn parse_items(v: &Value, id: &str) -> Result<Vec<ListItem>, TreeError> {
    let arr = v
        .get("items")
        .and_then(lua_array)
        .ok_or_else(|| bad_props(id, "items", "expected an array"))?;
    arr.iter()
        .map(|i| {
            let obj = i
                .as_object()
                .ok_or_else(|| bad_props(id, "items", "each item must be an object"))?;
            let item_id = obj
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_props(id, "items", "each item needs a string \"id\""))?;
            let label = obj
                .get("label")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_props(id, "items", "each item needs a string \"label\""))?;
            let selectable = match obj.get("selectable") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => {
                    return Err(bad_props(id, "items", "item \"selectable\" must be a boolean"))
                }
            };
            Ok(ListItem {
                id: item_id.to_string(),
                label: label.to_string(),
                selectable,
            })
        })
        .collect()
}

fn node_to_value(n: &Node) -> Value {
    let mut m = Map::new();
    m.insert("id".to_string(), Value::String(n.id.clone()));
    m.insert("kind".to_string(), Value::String(n.kind().as_str().to_string()));
    match &n.props {
        NodeProps::Stack { z } => {
            if *z != 0 {
                m.insert("z".to_string(), Value::from(*z));
            }
        }
        NodeProps::Row | NodeProps::Col => {}
        NodeProps::Text { spans } | NodeProps::Code { spans } => {
            m.insert(
                "spans".to_string(),
                serde_json::to_value(spans).expect("spans serialize"),
            );
        }
        NodeProps::Input { content } => {
            m.insert("content".to_string(), Value::String(content.clone()));
        }
        NodeProps::List { items } => {
            m.insert(
                "items".to_string(),
                serde_json::to_value(items).expect("items serialize"),
            );
        }
        NodeProps::Button { label } => {
            m.insert("label".to_string(), Value::String(label.clone()));
        }
        NodeProps::Layer { z, modal } => {
            if *z != 0 {
                m.insert("z".to_string(), Value::from(*z));
            }
            if *modal {
                m.insert("modal".to_string(), Value::Bool(true));
            }
        }
    }
    if n.disabled {
        m.insert("disabled".to_string(), Value::Bool(true));
    }
    if !n.children.is_empty() {
        m.insert(
            "children".to_string(),
            Value::Array(n.children.iter().map(node_to_value).collect()),
        );
    }
    Value::Object(m)
}

fn prefix_node(node: &Node, index: usize) -> Node {
    let mut prefixed = node.clone();
    prefixed.id = format!("{index}.{}", node.id);
    prefixed.children = node.children.iter().map(|c| prefix_node(c, index)).collect();
    prefixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_json() {
        let tree = SemanticTree::new(
            Node::stack("root").child(
                Node::input("input", "hi"),
            ),
        );
        let v = tree.to_json();
        let parsed = SemanticTree::from_json(&v).unwrap();
        assert_eq!(parsed, tree);
        assert!(parsed.is_focusable("input"));
    }

    #[test]
    fn round_trips_every_primitive() {
        let tree = SemanticTree::new(
            Node::stack("root")
                .child(Node::row("r").child(Node::col("c")))
                .child(Node::text_spans(
                    "t",
                    vec![Span::plain("a"), Span::styled("b", "header")],
                ))
                .child(Node::code("code", "x"))
                .child(Node::input("i", "draft"))
                .child(Node::list(
                    "l",
                    vec![ListItem::new("i1", "one").selectable()],
                ))
                .child(Node::button("b", "go"))
                .child(Node::layer("layer", 2, true)),
        );
        let parsed = SemanticTree::from_json(&tree.to_json()).unwrap();
        assert_eq!(parsed, tree);
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = SemanticTree::from_json(&json!({"root": {"id": "r", "kind": "carousel"}}))
            .unwrap_err();
        assert!(matches!(err, TreeError::UnknownKind { .. }));
    }

    #[test]
    fn rejects_bad_root_kind() {
        let err = SemanticTree::from_json(&json!({"root": {"id": "r", "kind": "col"}}))
            .unwrap_err();
        assert!(matches!(err, TreeError::BadRootKind(_)));
    }

    #[test]
    fn rejects_malformed_props() {
        // text without spans
        let err = SemanticTree::from_json(&json!({
            "root": {"id": "r", "kind": "stack", "children": [{"id": "t", "kind": "text"}]}
        }))
        .unwrap_err();
        assert!(matches!(err, TreeError::BadProps { prop: "spans", .. }));
        // list items with a non-string label
        let err = SemanticTree::from_json(&json!({
            "root": {"id": "r", "kind": "stack", "children": [
                {"id": "l", "kind": "list", "items": [{"id": "i", "label": 3}]}
            ]}
        }))
        .unwrap_err();
        assert!(matches!(err, TreeError::BadProps { prop: "items", .. }));
        // stack z of the wrong type
        let err = SemanticTree::from_json(&json!({
            "root": {"id": "r", "kind": "stack", "z": "high"}
        }))
        .unwrap_err();
        assert!(matches!(err, TreeError::BadProps { prop: "z", .. }));
    }

    #[test]
    fn empty_lua_tables_are_empty_arrays() {
        // The guest encoder renders an empty Lua table as `{}`, not `[]`.
        let parsed = SemanticTree::from_json(&json!({
            "root": {"id": "r", "kind": "stack", "children": [
                {"id": "t", "kind": "text", "spans": {}},
                {"id": "l", "kind": "list", "items": {}}
            ]}
        }))
        .unwrap();
        assert!(parsed.node("t").unwrap().spans().is_empty());
        assert!(parsed.node("l").unwrap().items().is_empty());
    }

    #[test]
    fn rejects_oversized_tree() {
        let mut node = json!({"id": "leaf", "kind": "text", "spans": [{"text": "x"}]});
        for _ in 0..MAX_TREE_DEPTH + 1 {
            node = json!({"id": "n", "kind": "stack", "children": [node]});
        }
        let err = SemanticTree::from_json(&json!({"root": node})).unwrap_err();
        assert!(matches!(err, TreeError::TooDeep { .. }));
    }

    #[test]
    fn rejects_too_many_nodes() {
        let children: Vec<Value> = (0..MAX_TREE_NODES)
            .map(|i| json!({"id": format!("t{i}"), "kind": "text", "spans": [{"text": "x"}]}))
            .collect();
        // root + MAX_TREE_NODES children exceeds the bound by one.
        let err = SemanticTree::from_json(&json!({
            "root": {"id": "r", "kind": "stack", "children": children}
        }))
        .unwrap_err();
        assert!(matches!(err, TreeError::TooManyNodes));
    }

    #[test]
    fn focusable_ring_order() {
        let tree = SemanticTree::new(
            Node::stack("root").child(
                Node::col("col")
                    .child(Node::button("a", "go"))
                    .child(Node::button("b", "no").disabled())
                    .child(Node::input("c", "hi")),
            ),
        );
        let ring: Vec<&str> = tree.focusable().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ring, vec!["a", "c"]);
        assert!(!tree.is_focusable("b"));
        assert_eq!(tree.input_node(Some("a")).unwrap().id, "c");
    }

    #[test]
    fn non_selectable_list_and_empty_button_are_not_focusable() {
        let tree = SemanticTree::new(
            Node::stack("root")
                .child(Node::list("display", vec![ListItem::new("i", "row")]))
                .child(Node::button("empty", "")),
        );
        assert!(tree.focusable().is_empty());

        let tree = SemanticTree::new(
            Node::stack("root").child(Node::list(
                "pick",
                vec![ListItem::new("i", "row").selectable()],
            )),
        );
        assert_eq!(tree.focusable().len(), 1);
    }

    #[test]
    fn sibling_z_changes_paint_order() {
        let tree = SemanticTree::new(
            Node::stack("root")
                .child(Node::styled_text("high", "high", "response"))
                .child(Node::styled_text("low", "low", "thought")),
        );
        // z is only carried by layout kinds; content kinds default to 0
        let parsed = SemanticTree::from_json(&tree.to_json()).unwrap();
        assert_eq!(parsed.node("high").unwrap().z(), 0);
        let layered = SemanticTree::new(
            Node::stack("root")
                .child(Node::stack_z("high", 3).child(Node::text("t1", "high")))
                .child(Node::stack_z("low", -1).child(Node::text("t2", "low"))),
        );
        let parsed = SemanticTree::from_json(&layered.to_json()).unwrap();
        assert_eq!(parsed.node("high").unwrap().z(), 3);
        assert_eq!(parsed.node("low").unwrap().z(), -1);
    }

    #[test]
    fn compose_prefixes_ids_and_keeps_roots() {
        let a = SemanticTree::new(
            Node::stack("root").child(Node::input("input", "a")),
        );
        let b = SemanticTree::new(
            Node::stack("root").child(Node::input("input", "b")),
        );
        // a single mount is returned verbatim (M5 byte-identical workbench)
        let solo = SemanticTree::compose(&[("main", &a)]);
        assert_eq!(solo.root.id, "root");
        assert_eq!(solo.focusable()[0].id, "input");
        let composite = SemanticTree::compose(&[("main", &a), ("status", &b)]);
        // synthetic root is not focusable; both mount roots are children
        assert_eq!(composite.root.id, "composite");
        assert_eq!(composite.root.kind(), NodeKind::Stack);
        assert_eq!(composite.root.children.len(), 2);
        assert_eq!(composite.root.children[0].id, "0.root");
        assert_eq!(composite.root.children[1].id, "1.root");
        // ids are unambiguous across mounts
        let ring: Vec<String> = composite.focusable().iter().map(|n| n.id.clone()).collect();
        assert_eq!(ring, vec!["0.input".to_string(), "1.input".to_string()]);
        // content is preserved per mount
        assert_eq!(composite.input_node(Some("1.input")).unwrap().content(), "b");
        assert_eq!(composite.input_node(None).unwrap().content(), "a");
        // round-trip resolution
        assert_eq!(SemanticTree::split_composite_id("1.input"), Some((1, "input")));
        assert_eq!(SemanticTree::split_composite_id("0.a.b"), Some((0, "a.b")));
        assert_eq!(SemanticTree::split_composite_id("noprefix"), None);
        // subtree() gives the within-mount ring
        let sub: Vec<&str> = composite
            .subtree("1.root")
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(sub, vec!["1.root", "1.input"]);
        assert!(composite.subtree("unknown").is_empty());
    }
}
