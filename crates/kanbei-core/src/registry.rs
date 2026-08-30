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

    /// Upcast by walking the chain registered for `kind` upward from `schema`:
    /// apply the upcaster at (kind, schema), then (kind, schema + 1), and so
    /// on until the next schema has no registered upcaster. A gap ends the
    /// chain at the last applied upcast. Ok(None) only when no upcaster
    /// exists at the record's own schema: the event stays
    /// opaque-but-inspectable. An upcaster error aborts the chain and
    /// propagates as Err.
    pub fn upcast(&self, kind: &str, schema: u32, payload: &Value) -> Result<Option<Value>, String> {
        let Some(first) = self.upcasters.get(&(kind.to_string(), schema)).copied() else {
            return Ok(None);
        };
        let mut cur = first(payload)?;
        let mut next_schema = schema + 1;
        while let Some(up) = self
            .upcasters
            .get(&(kind.to_string(), next_schema))
            .copied()
        {
            cur = up(&cur)?;
            next_schema += 1;
        }
        Ok(Some(cur))
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

/// Fixture upcaster: v2 user_message {text, role} -> v3 {text, role, channel:"default"}.
pub fn upcast_user_message_v2_to_v3(p: &Value) -> Result<Value, String> {
    let text = p
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or("user_message v2: missing text")?;
    let role = p
        .get("role")
        .and_then(|r| r.as_str())
        .ok_or("user_message v2: missing role")?;
    Ok(json!({ "text": text, "role": role, "channel": "default" }))
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

    #[test]
    fn upcast_walks_full_chain() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        r.register("user_message", 2, upcast_user_message_v2_to_v3)
            .unwrap();
        // v1 record upcasts v1 -> v2 -> v3 to the v3 shape
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
        // a v2 record starts at v2 and still reaches v3
        let v = r
            .upcast("user_message", 2, &json!({"text": "hi", "role": "user"}))
            .unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
    }

    #[test]
    fn upcast_single_hop_unchanged() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(v, Some(json!({"text": "hi", "role": "user"})));
    }

    #[test]
    fn upcast_chain_unknown_kind_and_schema_still_none() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        r.register("user_message", 2, upcast_user_message_v2_to_v3)
            .unwrap();
        // unknown kind despite a registered chain elsewhere
        assert_eq!(
            r.upcast("future_kind", 9, &json!({"mystery": 42})).unwrap(),
            None
        );
        // known kind, schema past the chain's head
        assert_eq!(r.upcast("user_message", 4, &json!({})).unwrap(), None);
    }

    #[test]
    fn upcast_chain_error_propagates_from_later_step() {
        let mut r = Registry::new();
        // first step succeeds but yields a v2 payload without a role
        r.register("user_message", 1, |_| Ok(json!({"text": "hi"}))).unwrap();
        r.register("user_message", 2, upcast_user_message_v2_to_v3)
            .unwrap();
        let err = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap_err();
        assert!(err.contains("missing role"));
    }

    #[test]
    fn upcast_chain_gap_ends_at_last_registered_schema() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        r.register("user_message", 3, upcast_user_message_v2_to_v3)
            .unwrap();
        // nothing registered at schema 2: the chain stops after v1 -> v2
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(v, Some(json!({"text": "hi", "role": "user"})));
        // schema 3 is registered but unreachable from a v1 record
        let v = r
            .upcast("user_message", 3, &json!({"text": "hi", "role": "user"}))
            .unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
    }
}
