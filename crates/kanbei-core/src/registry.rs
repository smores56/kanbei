//! Upcaster registry: version-pinned payload interpretation (S6 shape).
//! The kernel stores every envelope verbatim regardless of module schema;
//! typed interpretation is a projection layered on only when an upcaster is
//! registered for (kind, schema). Unknown kinds stay opaque-but-inspectable.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use thiserror::Error;

pub type Upcaster = fn(&Value) -> Result<Value, String>;

/// kind -> (payload schema, upcaster to the latest interpretation)
#[derive(Debug, Default)]
pub struct Registry {
    upcasters: BTreeMap<(String, u32), Upcaster>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, kind: &str, schema: u32, up: Upcaster) -> Result<(), RegistryError> {
        use std::collections::btree_map::Entry;
        match self.upcasters.entry((kind.to_string(), schema)) {
            Entry::Vacant(v) => {
                v.insert(up);
                Ok(())
            }
            Entry::Occupied(_) => Err(RegistryError::Duplicate {
                kind: kind.to_string(),
                schema,
            }),
        }
    }

    /// None = unknown kind/schema: the event stays opaque-but-inspectable.
    pub fn upcast(&self, kind: &str, schema: u32, payload: &Value) -> Result<Option<Value>, String> {
        match self.upcasters.get(&(kind.to_string(), schema)) {
            Some(up) => up(payload).map(Some),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    #[error("upcaster already registered for kind {kind:?} schema {schema}")]
    Duplicate { kind: String, schema: u32 },
}

// ---------- fixture upcasters (S6 shapes) ----------

/// Fixture upcaster: v1 user_message {text} -> v2 {text, role:"user"}.
pub fn upcast_user_message_v1_to_v2(p: &Value) -> Result<Value, String> {
    let text = p
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or("user_message v1: missing text")?;
    Ok(json!({ "text": text, "role": "user" }))
}

/// Fixture upcaster: v1 tool_result {tool, ok} -> v2 {tool, ok, summary}.
pub fn upcast_tool_result_v1_to_v2(p: &Value) -> Result<Value, String> {
    let tool = p
        .get("tool")
        .and_then(|t| t.as_str())
        .ok_or("tool_result v1: missing tool")?;
    let ok = p
        .get("ok")
        .and_then(|o| o.as_bool())
        .ok_or("tool_result v1: missing ok")?;
    Ok(json!({ "tool": tool, "ok": ok, "summary": if ok { "ok" } else { "failed" } }))
}

// ---------- reconstruction report (S6 shape) ----------

#[derive(Debug, Default)]
pub struct KindStat {
    pub schema: u32,
    pub count: u64,
    pub upcasted: u64,
    pub opaque: u64,
    pub opaque_reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub events: u64,
    pub kinds: BTreeMap<String, KindStat>,
    pub missing_objects: Vec<String>,
    pub upcast_errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upcast_known_returns_some() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(v, Some(json!({"text": "hi", "role": "user"})));

        r.register("tool_result", 1, upcast_tool_result_v1_to_v2)
            .unwrap();
        let v = r
            .upcast("tool_result", 1, &json!({"tool": "read_file", "ok": false}))
            .unwrap();
        assert_eq!(
            v,
            Some(json!({"tool": "read_file", "ok": false, "summary": "failed"}))
        );
    }

    #[test]
    fn upcast_unknown_returns_none() {
        let r = Registry::new();
        assert_eq!(r.upcast("future_kind", 9, &json!({"mystery": 42})).unwrap(), None);
        // known kind, unknown schema
        assert_eq!(r.upcast("user_message", 2, &json!({})).unwrap(), None);
    }

    #[test]
    fn duplicate_register_errors() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        let err = r
            .register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::Duplicate { kind, schema }
                if kind == "user_message" && schema == 1
        ));
    }

    #[test]
    fn upcast_error_propagates() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        let err = r.upcast("user_message", 1, &json!({})).unwrap_err();
        assert!(err.contains("missing text"));
    }
}
