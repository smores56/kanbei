//! Upcaster registry: version-pinned payload interpretation (S6 shape).
//! The kernel stores every envelope verbatim regardless of module schema;
//! typed interpretation is a projection layered on only when an upcaster is
//! registered for (kind, schema). Unknown kinds stay opaque-but-inspectable.
//!
//! Upcasters are either kernel Rust functions (fn pointers) or declarative
//! [`UpcastDescriptor`]s: bounded JSON transforms interpreted by the kernel
//! (architecture.md R-06/A-08) — never module-executed code on the
//! reconstruction path.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;

pub type Upcaster = fn(&Value) -> Result<Value, String>;

/// One entry in a kind's upcast chain: a kernel fn pointer or a declarative
/// descriptor. Fn entries step the walk to schema + 1; descriptor entries
/// jump it to their target schema.
#[derive(Debug, Clone)]
enum UpcastEntry {
    Fn(Upcaster),
    Descriptor(UpcastDescriptor),
}

impl UpcastEntry {
    fn next_schema(&self, schema: u32) -> u32 {
        match self {
            UpcastEntry::Fn(_) => schema + 1,
            UpcastEntry::Descriptor(d) => d.schema_target(),
        }
    }
}

/// kind -> (payload schema, upcaster to the latest interpretation)
#[derive(Debug, Default)]
pub struct Registry {
    upcasters: BTreeMap<(String, u32), UpcastEntry>,
    missing_packages: BTreeSet<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, kind: &str, schema: u32, up: Upcaster) -> Result<(), RegistryError> {
        use std::collections::btree_map::Entry;
        match self.upcasters.entry((kind.to_string(), schema)) {
            Entry::Vacant(v) => {
                v.insert(UpcastEntry::Fn(up));
                Ok(())
            }
            Entry::Occupied(_) => Err(RegistryError::Duplicate {
                kind: kind.to_string(),
                schema,
            }),
        }
    }

    /// Register a declarative descriptor as the upcaster for (kind, source).
    /// Same duplicate check as [`Self::register`]; the descriptor's target
    /// schema must be strictly greater than its source schema.
    pub fn register_descriptor(
        &mut self,
        kind: &str,
        source_schema: u32,
        descriptor: UpcastDescriptor,
    ) -> Result<(), RegistryError> {
        let target = descriptor.schema_target();
        if target <= source_schema {
            return Err(RegistryError::InvalidTargetSchema {
                kind: kind.to_string(),
                source_schema,
                target_schema: target,
            });
        }
        use std::collections::btree_map::Entry;
        match self.upcasters.entry((kind.to_string(), source_schema)) {
            Entry::Vacant(v) => {
                v.insert(UpcastEntry::Descriptor(descriptor));
                Ok(())
            }
            Entry::Occupied(_) => Err(RegistryError::Duplicate {
                kind: kind.to_string(),
                schema: source_schema,
            }),
        }
    }

    /// Upcast by walking the chain registered for `kind` upward from `schema`:
    /// fn entries apply and step to schema + 1, descriptor entries apply and
    /// jump to their target schema. A gap ends the chain at the last applied
    /// upcast. Ok(None) when no upcaster exists at the record's own schema or
    /// when a package the chain depends on was reported missing: the event
    /// stays opaque-but-inspectable and the rebuild continues. An upcaster
    /// error aborts the chain and propagates as Err.
    pub fn upcast(&self, kind: &str, schema: u32, payload: &Value) -> Result<Option<Value>, String> {
        let chain = self.chain(kind, schema);
        if chain.is_empty() {
            return Ok(None);
        }
        let mut cur = payload.clone();
        for (s, entry) in chain {
            if let UpcastEntry::Descriptor(d) = entry
                && let Some(pkg) = d.package()
                && self.missing_packages.contains(pkg)
            {
                // precise partial availability: opaque-but-inspectable,
                // the rebuild continues (architecture.md R-06/A-08)
                return Ok(None);
            }
            cur = match entry {
                UpcastEntry::Fn(f) => f(&cur)?,
                UpcastEntry::Descriptor(d) => d
                    .apply(&cur)
                    .map_err(|e| format!("{kind} schema {s}: {e}"))?,
            };
        }
        Ok(Some(cur))
    }

    /// The chain of entries for `kind` starting at `schema`: fn entries step
    /// +1, descriptors jump to their target schema; a gap ends the chain.
    fn chain(&self, kind: &str, schema: u32) -> Vec<(u32, &UpcastEntry)> {
        let mut out = Vec::new();
        let mut next = schema;
        while let Some(entry) = self.upcasters.get(&(kind.to_string(), next)) {
            out.push((next, entry));
            next = entry.next_schema(next);
        }
        out
    }

    /// Mark a package as unavailable; the caller reports this from its own
    /// package resolution (R-06 seam; projection mirrors `missing_objects`).
    /// Upcasts whose chain declares the package then return Ok(None).
    pub fn note_missing_package(&mut self, package: &str) {
        self.missing_packages.insert(package.to_string());
    }

    /// Precise opaque reason when a descriptor in the chain at (kind, schema)
    /// declares a package that was reported missing: names the package, kind,
    /// and record schema. None when every package the chain depends on is
    /// available (or none is declared).
    pub fn missing_package_reason(&self, kind: &str, schema: u32) -> Option<String> {
        for (_, entry) in self.chain(kind, schema) {
            if let UpcastEntry::Descriptor(d) = entry
                && let Some(pkg) = d.package()
                && self.missing_packages.contains(pkg)
            {
                return Some(format!(
                    "package {pkg:?} unavailable for kind {kind:?} schema {schema}"
                ));
            }
        }
        None
    }

    /// The package ref declared by the descriptor at (kind, schema), if any.
    pub fn descriptor_package(&self, kind: &str, schema: u32) -> Option<&str> {
        match self.upcasters.get(&(kind.to_string(), schema))? {
            UpcastEntry::Descriptor(d) => d.package(),
            UpcastEntry::Fn(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    #[error("upcaster already registered for kind {kind:?} schema {schema}")]
    Duplicate { kind: String, schema: u32 },
    #[error("descriptor target schema {target_schema} must be greater than source schema {source_schema} for kind {kind:?}")]
    InvalidTargetSchema {
        kind: String,
        source_schema: u32,
        target_schema: u32,
    },
}

// ---------- declarative upcaster descriptors (M9 wave 2) ----------

/// A bounded, declarative payload transform interpreted by the kernel
/// (architecture.md R-06/A-08): never module-executed code on the
/// reconstruction path. A descriptor upgrades a payload from its source
/// schema (the registry key it is registered under) to `target_schema`,
/// which must be strictly greater.
///
/// JSON format (all fields optional except `target_schema` and `ops`):
/// ```json
/// {
///   "target_schema": 3,
///   "package": "optional-package-or-digest-ref",
///   "require": ["text", "role"],
///   "ops": [
///     {"add": {"channel": "default"}},
///     {"set": {"text": "fixed"}},
///     {"rename": {"from": "old", "to": "new"}},
///     {"remove": ["secret"]},
///     {"wrap": "outer"},
///     {"unwrap": "inner"},
///     {"map": {"from": "ok", "to": "summary", "cases": {"true": "ok", "false": "failed"}}}
///   ]
/// }
/// ```
///
/// Ops apply in order, each deterministically transforming the payload:
/// - `add`: insert a constant field only when absent (dotted paths create
///   intermediate objects).
/// - `set`: overwrite a field with a constant (intermediates created).
/// - `rename`: move the value at `from` to `to`; missing `from` errors.
/// - `remove`: drop fields; absent paths are a no-op.
/// - `wrap`: move the whole payload under a (dotted) key.
/// - `unwrap`: lift a key's value to the top level; missing key errors.
/// - `map`: replace the value at `to` with the case-table constant matching
///   the value at `from`; case keys are canonical JSON encodings of the
///   matched value (e.g. `"true"` for boolean true), and a value with no
///   matching case errors.
///
/// Paths address nested fields with `.` (e.g. `"a.b"`); segments must be
/// non-empty. Unknown ops, unknown op fields, malformed shapes, and invalid
/// paths are rejected at parse time (fail-closed, never ignored).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpcastDescriptor {
    target_schema: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    require: Vec<String>,
    ops: Vec<Op>,
}

impl UpcastDescriptor {
    /// Parse and validate a descriptor. Rejects unknown ops, unknown op
    /// fields, malformed shapes, and invalid paths with a typed error naming
    /// the offending op/path (fail-closed). The source schema is not known
    /// here; `target_schema > source` is enforced at registration.
    pub fn parse(s: &str) -> Result<UpcastDescriptor, DescriptorError> {
        let v: Value = serde_json::from_str(s).map_err(|e| DescriptorError::Json(e.to_string()))?;
        UpcastDescriptor::from_value(v)
    }

    /// The schema version this descriptor upgrades TO.
    pub fn schema_target(&self) -> u32 {
        self.target_schema
    }

    /// The optional package/digest ref this descriptor belongs to (R-06 seam
    /// for later package pinning).
    pub fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    /// Apply the transform to `payload`: `require` preconditions first, then
    /// the ops in order. Pure and deterministic.
    pub fn apply(&self, payload: &Value) -> Result<Value, String> {
        for path in &self.require {
            if resolve(payload, path).is_none() {
                return Err(format!("missing required field '{path}'"));
            }
        }
        let mut cur = payload.clone();
        for op in &self.ops {
            apply_op(&mut cur, op)?;
        }
        Ok(cur)
    }

    fn from_value(v: Value) -> Result<UpcastDescriptor, DescriptorError> {
        let obj = v.as_object().ok_or(DescriptorError::NotAnObject)?;
        let mut target_schema = None;
        let mut package = None;
        let mut require = Vec::new();
        let mut ops = None;
        for (key, val) in obj {
            match key.as_str() {
                "target_schema" => {
                    let n = val
                        .as_u64()
                        .filter(|n| *n > 0)
                        .ok_or(DescriptorError::InvalidTargetSchema)?;
                    target_schema = Some(n as u32);
                }
                "package" => {
                    let s = val
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or(DescriptorError::InvalidPackage)?;
                    package = Some(s.to_string());
                }
                "require" => {
                    let arr = val.as_array().ok_or(DescriptorError::InvalidRequire)?;
                    for item in arr {
                        let s = item.as_str().ok_or(DescriptorError::InvalidRequire)?;
                        validate_path(s)?;
                        require.push(s.to_string());
                    }
                }
                "ops" => ops = Some(val.clone()),
                other => return Err(DescriptorError::UnknownField { field: other.to_string() }),
            }
        }
        let target_schema = target_schema.ok_or(DescriptorError::MissingTargetSchema)?;
        let ops = ops.ok_or(DescriptorError::MissingOps)?;
        let arr = ops.as_array().ok_or(DescriptorError::InvalidOps)?;
        let mut parsed = Vec::with_capacity(arr.len());
        for step in arr {
            parsed.push(parse_op(step)?);
        }
        Ok(UpcastDescriptor {
            target_schema,
            package,
            require,
            ops: parsed,
        })
    }
}

/// Deserializing re-validates through [`UpcastDescriptor::from_value`], so
/// persisted descriptors can only round-trip into a valid state
/// (fail-closed).
impl<'de> Deserialize<'de> for UpcastDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        UpcastDescriptor::from_value(v).map_err(serde::de::Error::custom)
    }
}

/// The closed set of transform ops a descriptor may contain. Externally
/// tagged so each op serializes as a single-key object, e.g.
/// `{"add": {"role": "user"}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Op {
    Add(BTreeMap<String, Value>),
    Set(BTreeMap<String, Value>),
    Rename { from: String, to: String },
    Remove(Vec<String>),
    Wrap(String),
    Unwrap(String),
    Map {
        from: String,
        to: String,
        cases: BTreeMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DescriptorError {
    #[error("upcast descriptor is not valid JSON: {0}")]
    Json(String),
    #[error("upcast descriptor must be a JSON object")]
    NotAnObject,
    #[error("upcast descriptor missing field 'target_schema'")]
    MissingTargetSchema,
    #[error("upcast descriptor field 'target_schema' must be a positive integer")]
    InvalidTargetSchema,
    #[error("upcast descriptor unknown field {field:?}")]
    UnknownField { field: String },
    #[error("upcast descriptor missing field 'ops'")]
    MissingOps,
    #[error("upcast descriptor field 'ops' must be an array of op steps")]
    InvalidOps,
    #[error("upcast descriptor op step must be a single-key object, got keys {keys:?}")]
    MalformedStep { keys: Vec<String> },
    #[error("upcast descriptor unknown op {op:?}")]
    UnknownOp { op: String },
    #[error("upcast descriptor op {op:?}: invalid shape (expected {expected})")]
    InvalidOpShape { op: String, expected: &'static str },
    #[error("upcast descriptor op {op:?}: unknown field {field:?}")]
    UnknownOpField { op: String, field: String },
    #[error("upcast descriptor op {op:?}: missing field {field:?}")]
    MissingOpField { op: String, field: String },
    #[error("upcast descriptor op {op:?}: case key {key:?} is not canonical JSON")]
    InvalidCaseKey { op: String, key: String },
    #[error("upcast descriptor invalid path {path:?}")]
    InvalidPath { path: String },
    #[error("upcast descriptor field 'require' must be an array of strings")]
    InvalidRequire,
    #[error("upcast descriptor field 'package' must be a non-empty string")]
    InvalidPackage,
}

fn validate_path(path: &str) -> Result<(), DescriptorError> {
    if path.is_empty() || path.split('.').any(|seg| seg.is_empty()) {
        return Err(DescriptorError::InvalidPath {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn parse_op(step: &Value) -> Result<Op, DescriptorError> {
    let obj = step
        .as_object()
        .ok_or_else(|| DescriptorError::MalformedStep { keys: Vec::new() })?;
    if obj.len() != 1 {
        return Err(DescriptorError::MalformedStep {
            keys: obj.keys().cloned().collect(),
        });
    }
    let (name, val) = obj.iter().next().expect("single-key op step");
    match name.as_str() {
        "add" | "set" => {
            let fields = val.as_object().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "an object of field -> constant",
            })?;
            let mut map = BTreeMap::new();
            for (path, constant) in fields {
                validate_path(path)?;
                map.insert(path.clone(), constant.clone());
            }
            Ok(if name == "add" {
                Op::Add(map)
            } else {
                Op::Set(map)
            })
        }
        "rename" => {
            let inner = val.as_object().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "an object with 'from' and 'to' paths",
            })?;
            let mut from = None;
            let mut to = None;
            for (key, v) in inner {
                match key.as_str() {
                    "from" => from = Some(v),
                    "to" => to = Some(v),
                    other => {
                        return Err(DescriptorError::UnknownOpField {
                            op: name.clone(),
                            field: other.to_string(),
                        })
                    }
                }
            }
            let from = from.ok_or_else(|| DescriptorError::MissingOpField {
                op: name.clone(),
                field: "from".to_string(),
            })?;
            let to = to.ok_or_else(|| DescriptorError::MissingOpField {
                op: name.clone(),
                field: "to".to_string(),
            })?;
            let from = from.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "string 'from' path",
            })?;
            let to = to.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "string 'to' path",
            })?;
            validate_path(from)?;
            validate_path(to)?;
            Ok(Op::Rename {
                from: from.to_string(),
                to: to.to_string(),
            })
        }
        "remove" => {
            let arr = val.as_array().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "an array of paths",
            })?;
            let mut paths = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                    op: name.clone(),
                    expected: "an array of paths",
                })?;
                validate_path(s)?;
                paths.push(s.to_string());
            }
            Ok(Op::Remove(paths))
        }
        "wrap" | "unwrap" => {
            let s = val.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "a path string",
            })?;
            validate_path(s)?;
            Ok(if name == "wrap" {
                Op::Wrap(s.to_string())
            } else {
                Op::Unwrap(s.to_string())
            })
        }
        "map" => {
            let inner = val.as_object().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "an object with 'from', 'to', and 'cases'",
            })?;
            let mut from = None;
            let mut to = None;
            let mut cases = None;
            for (key, v) in inner {
                match key.as_str() {
                    "from" => from = Some(v),
                    "to" => to = Some(v),
                    "cases" => cases = Some(v),
                    other => {
                        return Err(DescriptorError::UnknownOpField {
                            op: name.clone(),
                            field: other.to_string(),
                        })
                    }
                }
            }
            let from = from.ok_or_else(|| DescriptorError::MissingOpField {
                op: name.clone(),
                field: "from".to_string(),
            })?;
            let to = to.ok_or_else(|| DescriptorError::MissingOpField {
                op: name.clone(),
                field: "to".to_string(),
            })?;
            let cases = cases.ok_or_else(|| DescriptorError::MissingOpField {
                op: name.clone(),
                field: "cases".to_string(),
            })?;
            let from = from.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "string 'from' path",
            })?;
            let to = to.as_str().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "string 'to' path",
            })?;
            validate_path(from)?;
            validate_path(to)?;
            let table = cases.as_object().ok_or_else(|| DescriptorError::InvalidOpShape {
                op: name.clone(),
                expected: "a non-empty case table",
            })?;
            if table.is_empty() {
                return Err(DescriptorError::InvalidOpShape {
                    op: name.clone(),
                    expected: "a non-empty case table",
                });
            }
            let mut map = BTreeMap::new();
            for (key, constant) in table {
                let parsed: Value = serde_json::from_str(key).map_err(|_| {
                    DescriptorError::InvalidCaseKey {
                        op: name.clone(),
                        key: key.clone(),
                    }
                })?;
                let canonical = serde_json::to_string(&parsed).map_err(|_| {
                    DescriptorError::InvalidCaseKey {
                        op: name.clone(),
                        key: key.clone(),
                    }
                })?;
                if canonical != *key {
                    return Err(DescriptorError::InvalidCaseKey {
                        op: name.clone(),
                        key: key.clone(),
                    });
                }
                map.insert(canonical, constant.clone());
            }
            Ok(Op::Map {
                from: from.to_string(),
                to: to.to_string(),
                cases: map,
            })
        }
        other => Err(DescriptorError::UnknownOp {
            op: other.to_string(),
        }),
    }
}

/// Resolve a dotted path; None when any segment is missing or not an object.
fn resolve<'v>(v: &'v Value, path: &str) -> Option<&'v Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Set `val` at a dotted path, creating intermediate objects. Errors when an
/// intermediate segment exists but is not an object.
fn set_path(cur: &mut Value, path: &str, val: Value) -> Result<(), String> {
    let segs: Vec<&str> = path.split('.').collect();
    let mut node = cur;
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        let obj = node.as_object_mut().ok_or_else(|| {
            format!("cannot set path '{path}': segment '{seg}' is not an object")
        })?;
        if last {
            obj.insert((*seg).to_string(), val);
            return Ok(());
        }
        node = obj.entry(*seg).or_insert_with(|| Value::Object(Map::new()));
    }
    unreachable!("paths with zero segments are rejected at parse time")
}

/// Remove the value at a dotted path; returns whether anything was removed.
/// Absent paths are a no-op.
fn remove_path(cur: &mut Value, path: &str) -> bool {
    let segs: Vec<&str> = path.split('.').collect();
    let mut node = cur;
    for (i, seg) in segs.iter().enumerate() {
        let Some(obj) = node.as_object_mut() else {
            return false;
        };
        if i == segs.len() - 1 {
            return obj.remove(*seg).is_some();
        }
        let Some(next) = obj.get_mut(*seg) else {
            return false;
        };
        node = next;
    }
    false
}

fn apply_op(cur: &mut Value, op: &Op) -> Result<(), String> {
    match op {
        Op::Add(fields) => {
            for (path, constant) in fields {
                if resolve(cur, path).is_none() {
                    set_path(cur, path, constant.clone())?;
                }
            }
        }
        Op::Set(fields) => {
            for (path, constant) in fields {
                set_path(cur, path, constant.clone())?;
            }
        }
        Op::Rename { from, to } => {
            let val = resolve(cur, from)
                .ok_or_else(|| format!("rename: source path '{from}' not found"))?
                .clone();
            remove_path(cur, from);
            set_path(cur, to, val)?;
        }
        Op::Remove(paths) => {
            for path in paths {
                remove_path(cur, path);
            }
        }
        Op::Wrap(key) => {
            let mut wrapped = Value::Object(Map::new());
            let old = std::mem::take(cur);
            set_path(&mut wrapped, key, old)?;
            *cur = wrapped;
        }
        Op::Unwrap(key) => {
            let val = resolve(cur, key)
                .ok_or_else(|| format!("unwrap: path '{key}' not found"))?
                .clone();
            *cur = val;
        }
        Op::Map { from, to, cases } => {
            let key = serde_json::to_string(
                resolve(cur, from)
                    .ok_or_else(|| format!("map: source path '{from}' not found"))?,
            )
            .map_err(|e| format!("map: serialize value at '{from}': {e}"))?;
            let constant = cases
                .get(&key)
                .ok_or_else(|| format!("map: no case for value {key} at path '{from}'"))?
                .clone();
            set_path(cur, to, constant)?;
        }
    }
    Ok(())
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
    /// Package/digest ref the chain's descriptor declares, when any (R-06
    /// seam for later package pinning).
    pub descriptor_package: Option<String>,
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

    // ---------- declarative upcaster descriptors ----------

    #[test]
    fn descriptor_add_sets_absent_fields_only() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"add": {"role": "user", "meta": {"origin": "cli"}}}]}"#,
        )
        .unwrap();
        let v = d.apply(&json!({"text": "hi"})).unwrap();
        assert_eq!(
            v,
            json!({"text": "hi", "role": "user", "meta": {"origin": "cli"}})
        );
        // existing fields are left untouched (add, not set)
        let v = d.apply(&json!({"role": "admin"})).unwrap();
        assert_eq!(v, json!({"role": "admin", "meta": {"origin": "cli"}}));
        // nested add creates intermediate objects
        let d = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"add": {"a.b": 1}}]}"#)
            .unwrap();
        let v = d.apply(&json!({})).unwrap();
        assert_eq!(v, json!({"a": {"b": 1}}));
    }

    #[test]
    fn descriptor_set_overwrites_and_creates_nested() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"set": {"text": "fixed"}}, {"set": {"a.b": 2}}]}"#,
        )
        .unwrap();
        let v = d.apply(&json!({"text": "hi", "a": {"b": 1}})).unwrap();
        assert_eq!(v, json!({"text": "fixed", "a": {"b": 2}}));
        // intermediate creation from an empty payload
        let v = d.apply(&json!({})).unwrap();
        assert_eq!(v, json!({"text": "fixed", "a": {"b": 2}}));
    }

    #[test]
    fn descriptor_rename_moves_and_errors_when_source_missing() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"rename": {"from": "text", "to": "content"}}]}"#,
        )
        .unwrap();
        let v = d.apply(&json!({"text": "hi", "role": "user"})).unwrap();
        assert_eq!(v, json!({"content": "hi", "role": "user"}));
        let err = d.apply(&json!({"role": "user"})).unwrap_err();
        assert!(err.contains("'text'"), "{err}");
    }

    #[test]
    fn descriptor_remove_drops_fields_absent_is_noop() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"remove": ["secret", "a.b"]}]}"#,
        )
        .unwrap();
        let v = d
            .apply(&json!({"text": "hi", "secret": 1, "a": {"b": 2, "c": 3}}))
            .unwrap();
        assert_eq!(v, json!({"text": "hi", "a": {"c": 3}}));
        // absent paths are a no-op
        let v = d.apply(&json!({"text": "hi"})).unwrap();
        assert_eq!(v, json!({"text": "hi"}));
    }

    #[test]
    fn descriptor_wrap_moves_payload_under_key() {
        let d = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"wrap": "outer"}]}"#)
            .unwrap();
        let v = d.apply(&json!({"text": "hi"})).unwrap();
        assert_eq!(v, json!({"outer": {"text": "hi"}}));
        // dotted wrap keys nest
        let d = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"wrap": "a.b"}]}"#)
            .unwrap();
        let v = d.apply(&json!({"text": "hi"})).unwrap();
        assert_eq!(v, json!({"a": {"b": {"text": "hi"}}}));
    }

    #[test]
    fn descriptor_unwrap_lifts_key_to_top() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"unwrap": "user_message"}]}"#,
        )
        .unwrap();
        let v = d
            .apply(&json!({"user_message": {"text": "hi"}, "meta": 1}))
            .unwrap();
        assert_eq!(v, json!({"text": "hi"}));
        let err = d.apply(&json!({"meta": 1})).unwrap_err();
        assert!(err.contains("'user_message'"), "{err}");
    }

    #[test]
    fn descriptor_map_looks_up_case_table() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"map": {"from": "ok", "to": "summary", "cases": {"true": "ok", "false": "failed"}}}]}"#,
        )
        .unwrap();
        let v = d.apply(&json!({"ok": true})).unwrap();
        assert_eq!(v, json!({"ok": true, "summary": "ok"}));
        let v = d.apply(&json!({"ok": false})).unwrap();
        assert_eq!(v, json!({"ok": false, "summary": "failed"}));
        // no matching case errors explicitly
        let err = d.apply(&json!({"ok": "maybe"})).unwrap_err();
        assert!(err.contains("no case"), "{err}");
        let err = d.apply(&json!({})).unwrap_err();
        assert!(err.contains("'ok'"), "{err}");
    }

    #[test]
    fn descriptor_require_checks_input_fields() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "require": ["text", "role"], "ops": [{"add": {"channel": "default"}}]}"#,
        )
        .unwrap();
        let v = d.apply(&json!({"text": "hi", "role": "user"})).unwrap();
        assert_eq!(v, json!({"text": "hi", "role": "user", "channel": "default"}));
        let err = d.apply(&json!({"text": "hi"})).unwrap_err();
        assert_eq!(err, "missing required field 'role'");
    }

    #[test]
    fn descriptor_parse_rejects_unknown_op() {
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"explode": {"x": 1}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::UnknownOp { op } if op == "explode"
        ));
    }

    #[test]
    fn descriptor_parse_rejects_unknown_fields() {
        // unknown top-level field
        let err = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [], "bogus": 1}"#)
            .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::UnknownField { field } if field == "bogus"
        ));
        // two ops in one step
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"add": {"a": 1}, "remove": ["b"]}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::MalformedStep { keys } if keys == vec!["add", "remove"]
        ));
        // unknown field inside rename
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"rename": {"from": "a", "to": "b", "x": 1}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::UnknownOpField { op, field } if op == "rename" && field == "x"
        ));
        // unknown field inside map
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"map": {"from": "a", "to": "b", "cases": {}, "x": 1}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::UnknownOpField { op, field } if op == "map" && field == "x"
        ));
    }

    #[test]
    fn descriptor_parse_rejects_malformed_shapes() {
        assert!(matches!(
            UpcastDescriptor::parse("[1, 2]").unwrap_err(),
            DescriptorError::NotAnObject
        ));
        assert!(matches!(
            UpcastDescriptor::parse("not json").unwrap_err(),
            DescriptorError::Json(_)
        ));
        // add takes an object of constants
        let err =
            UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"add": [1, 2]}]}"#)
                .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::InvalidOpShape { op, .. } if op == "add"
        ));
        // remove takes an array of paths
        let err = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"remove": [1]}]}"#)
            .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::InvalidOpShape { op, .. } if op == "remove"
        ));
        // rename requires both from and to
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"rename": {"from": "a"}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::MissingOpField { op, field } if op == "rename" && field == "to"
        ));
        // map requires from, to, and a non-empty case table
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"map": {"from": "a", "to": "b"}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::MissingOpField { op, field } if op == "map" && field == "cases"
        ));
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"map": {"from": "a", "to": "b", "cases": {}}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::InvalidOpShape { op, .. } if op == "map"
        ));
        // case keys must be canonical JSON encodings
        let err = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "ops": [{"map": {"from": "a", "to": "b", "cases": {"true": "yes", "nope": "no"}}}]}"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DescriptorError::InvalidCaseKey { op, key } if op == "map" && key == "nope"
        ));
        // missing or wrong target_schema / ops
        assert!(matches!(
            UpcastDescriptor::parse(r#"{"ops": []}"#).unwrap_err(),
            DescriptorError::MissingTargetSchema
        ));
        assert!(matches!(
            UpcastDescriptor::parse(r#"{"target_schema": 0, "ops": []}"#).unwrap_err(),
            DescriptorError::InvalidTargetSchema
        ));
        assert!(matches!(
            UpcastDescriptor::parse(r#"{"target_schema": "x", "ops": []}"#).unwrap_err(),
            DescriptorError::InvalidTargetSchema
        ));
        assert!(matches!(
            UpcastDescriptor::parse(r#"{"target_schema": 2}"#).unwrap_err(),
            DescriptorError::MissingOps
        ));
        assert!(matches!(
            UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": "nope"}"#).unwrap_err(),
            DescriptorError::InvalidOps
        ));
    }

    #[test]
    fn descriptor_parse_rejects_invalid_paths() {
        for (desc, path) in [
            (r#"{"target_schema": 2, "ops": [{"add": {"a..b": 1}}]}"#, "a..b"),
            (r#"{"target_schema": 2, "ops": [{"add": {".a": 1}}]}"#, ".a"),
            (r#"{"target_schema": 2, "ops": [{"add": {"a.": 1}}]}"#, "a."),
            (r#"{"target_schema": 2, "ops": [{"add": {"": 1}}]}"#, ""),
        ] {
            let err = UpcastDescriptor::parse(desc).unwrap_err();
            assert!(
                matches!(err, DescriptorError::InvalidPath { ref path } if path == path),
                "expected InvalidPath for {path:?}"
            );
        }
        // require, wrap, unwrap, rename, and remove paths are validated too
        for desc in [
            r#"{"target_schema": 2, "require": ["a..b"], "ops": []}"#,
            r#"{"target_schema": 2, "ops": [{"wrap": "a..b"}]}"#,
            r#"{"target_schema": 2, "ops": [{"unwrap": "a."}]}"#,
            r#"{"target_schema": 2, "ops": [{"rename": {"from": "a", "to": "b..c"}}]}"#,
            r#"{"target_schema": 2, "ops": [{"remove": ["a..b"]}]}"#,
        ] {
            let err = UpcastDescriptor::parse(desc).unwrap_err();
            assert!(matches!(err, DescriptorError::InvalidPath { .. }), "{err}");
        }
    }

    #[test]
    fn descriptor_register_rejects_target_at_or_below_source() {
        let mut r = Registry::new();
        for (source, target) in [(1, 1), (2, 1), (3, 2)] {
            let d = UpcastDescriptor::parse(&format!(
                r#"{{"target_schema": {target}, "ops": []}}"#
            ))
            .unwrap();
            let err = r.register_descriptor("k", source, d).unwrap_err();
            assert!(matches!(
                err,
                RegistryError::InvalidTargetSchema {
                    kind,
                    source_schema,
                    target_schema
                } if kind == "k" && source_schema == source && target_schema == target
            ));
        }
        // strictly greater passes
        let d = UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": []}"#).unwrap();
        assert!(r.register_descriptor("k", 1, d).is_ok());
    }

    #[test]
    fn descriptor_duplicate_register_errors() {
        let mut r = Registry::new();
        let d = || {
            UpcastDescriptor::parse(r#"{"target_schema": 2, "ops": [{"add": {"a": 1}}]}"#).unwrap()
        };
        r.register_descriptor("k", 1, d()).unwrap();
        let err = r.register_descriptor("k", 1, d()).unwrap_err();
        assert!(matches!(
            err,
            RegistryError::Duplicate { kind, schema } if kind == "k" && schema == 1
        ));
        // a descriptor clashes with an fn entry and vice versa
        let err = r.register("k", 1, upcast_user_message_v1_to_v2).unwrap_err();
        assert!(matches!(err, RegistryError::Duplicate { .. }));
    }

    #[test]
    fn descriptor_mixed_fn_chain_walks_target_schemas() {
        let mut r = Registry::new();
        r.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "require": ["text", "role"], "ops": [{"add": {"channel": "default"}}]}"#,
        )
        .unwrap();
        r.register_descriptor("user_message", 2, d).unwrap();
        // from 1: fn v1 -> v2, then the descriptor 2 -> 3; lands on the v3 shape
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
        // from 2: the descriptor alone
        let v = r
            .upcast("user_message", 2, &json!({"text": "hi", "role": "user"}))
            .unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
        // a fn registered at the descriptor's target schema is picked up
        // next: the walk looks up schema 3 after the descriptor applies
        r.register("user_message", 3, |p| Ok(json!({"wrapped": p}))).unwrap();
        let v = r
            .upcast("user_message", 2, &json!({"text": "hi", "role": "user"}))
            .unwrap();
        assert_eq!(
            v,
            Some(json!({"wrapped": {"text": "hi", "role": "user", "channel": "default"}}))
        );
    }

    #[test]
    fn descriptor_error_aborts_chain() {
        let mut r = Registry::new();
        r.register("user_message", 1, |_| Ok(json!({"text": "hi"}))).unwrap();
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "require": ["text", "role"], "ops": [{"add": {"channel": "default"}}]}"#,
        )
        .unwrap();
        r.register_descriptor("user_message", 2, d).unwrap();
        let err = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap_err();
        assert!(err.contains("missing required field 'role'"), "{err}");
        // descriptor errors carry kind/schema context for the report
        assert!(err.contains("user_message"), "{err}");
    }

    #[test]
    fn descriptor_chain_gap_ends_at_last_applied() {
        let mut r = Registry::new();
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "require": ["text"], "ops": [{"add": {"role": "user"}}, {"add": {"channel": "default"}}]}"#,
        )
        .unwrap();
        r.register_descriptor("user_message", 1, d).unwrap();
        // an entry at schema 2 is skipped: the descriptor jumps 1 -> 3
        r.register("user_message", 2, upcast_user_message_v2_to_v3)
            .unwrap();
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(
            v,
            Some(json!({"text": "hi", "role": "user", "channel": "default"}))
        );
        // nothing at schema 3: the chain ends at the descriptor's target
        let mut r2 = Registry::new();
        r2.register_descriptor(
            "user_message",
            1,
            UpcastDescriptor::parse(r#"{"target_schema": 3, "ops": [{"add": {"role": "user"}}]}"#)
                .unwrap(),
        )
        .unwrap();
        let v = r2.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(v, Some(json!({"text": "hi", "role": "user"})));
    }

    #[test]
    fn descriptor_missing_package_returns_none_with_reason() {
        let mut r = Registry::new();
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "package": "upcast-msg@1.0.0", "ops": [{"add": {"role": "user"}}]}"#,
        )
        .unwrap();
        r.register_descriptor("user_message", 1, d).unwrap();
        assert_eq!(
            r.descriptor_package("user_message", 1),
            Some("upcast-msg@1.0.0")
        );
        assert_eq!(r.descriptor_package("user_message", 2), None);
        // package available: upcasts normally
        let v = r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap();
        assert_eq!(v, Some(json!({"text": "hi", "role": "user"})));
        // caller reports the package missing: the record stays opaque with a
        // precise reason, no upcaster error, the rebuild continues
        r.note_missing_package("upcast-msg@1.0.0");
        assert_eq!(r.upcast("user_message", 1, &json!({"text": "hi"})).unwrap(), None);
        let reason = r.missing_package_reason("user_message", 1).unwrap();
        assert!(reason.contains("upcast-msg@1.0.0"), "{reason}");
        assert!(reason.contains("user_message"), "{reason}");
        assert!(reason.contains("schema 1"), "{reason}");
        // unknown kinds and fns-only chains report no missing package
        assert_eq!(r.missing_package_reason("tool_result", 1), None);
    }

    #[test]
    fn descriptor_apply_is_deterministic() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "require": ["text"], "ops": [{"add": {"role": "user"}}, {"rename": {"from": "role", "to": "actor"}}, {"set": {"actor": "assistant"}}, {"remove": ["old"]}, {"wrap": "msg"}]}"#,
        )
        .unwrap();
        let p = json!({"text": "hi", "old": 1});
        let a = d.apply(&p).unwrap();
        let b = d.apply(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a,
            json!({"msg": {"text": "hi", "actor": "assistant"}})
        );
    }

    #[test]
    fn descriptor_equivalence_with_fixture_upcasters() {
        // user_message v1 -> v2
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "require": ["text"], "ops": [{"add": {"role": "user"}}]}"#,
        )
        .unwrap();
        for p in [json!({"text": "hi"}), json!({"text": "hello"})] {
            assert_eq!(d.apply(&p), upcast_user_message_v1_to_v2(&p));
        }
        // user_message v2 -> v3
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "require": ["text", "role"], "ops": [{"add": {"channel": "default"}}]}"#,
        )
        .unwrap();
        let p = json!({"text": "hi", "role": "user"});
        assert_eq!(d.apply(&p), upcast_user_message_v2_to_v3(&p));
        // tool_result v1 -> v2: the fixture's conditional summary is a case
        // table
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 2, "require": ["tool", "ok"], "ops": [{"map": {"from": "ok", "to": "summary", "cases": {"true": "ok", "false": "failed"}}}]}"#,
        )
        .unwrap();
        for p in [
            json!({"tool": "read_file", "ok": true}),
            json!({"tool": "read_file", "ok": false}),
        ] {
            assert_eq!(d.apply(&p), upcast_tool_result_v1_to_v2(&p));
        }
        // full chains through the registry produce identical output
        let mut desc = Registry::new();
        desc.register_descriptor(
            "user_message",
            1,
            UpcastDescriptor::parse(
                r#"{"target_schema": 2, "require": ["text"], "ops": [{"add": {"role": "user"}}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        desc.register_descriptor(
            "user_message",
            2,
            UpcastDescriptor::parse(
                r#"{"target_schema": 3, "require": ["text", "role"], "ops": [{"add": {"channel": "default"}}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let mut fns = Registry::new();
        fns.register("user_message", 1, upcast_user_message_v1_to_v2)
            .unwrap();
        fns.register("user_message", 2, upcast_user_message_v2_to_v3)
            .unwrap();
        assert_eq!(
            desc.upcast("user_message", 1, &json!({"text": "hi"})).unwrap(),
            fns.upcast("user_message", 1, &json!({"text": "hi"})).unwrap()
        );
    }

    #[test]
    fn descriptor_serde_roundtrip() {
        let d = UpcastDescriptor::parse(
            r#"{"target_schema": 3, "package": "pk@1", "require": ["text"], "ops": [{"add": {"role": "user"}}, {"rename": {"from": "role", "to": "actor"}}, {"remove": ["old"]}, {"map": {"from": "ok", "to": "summary", "cases": {"true": "ok", "false": "failed"}}}]}"#,
        )
        .unwrap();
        let s = serde_json::to_string(&d).unwrap();
        // round-trips through the validated parse path and through
        // Deserialize (which re-validates)
        assert_eq!(UpcastDescriptor::parse(&s).unwrap(), d);
        let d2: UpcastDescriptor = serde_json::from_str(&s).unwrap();
        assert_eq!(d2, d);
        // the serialized form is itself a valid descriptor document
        assert!(serde_json::from_str::<Value>(&s).unwrap().is_object());
    }
}
