//! S6 spike: version-pinned reconstruction across a kernel-upgrade fixture.
//! Disposable spike code — never promoted into the implementation.
//!
//! Per R-06: the kernel validates and stores every event envelope invariant
//! regardless of module schema; module-custom payloads are retained verbatim;
//! typed interpretation is a projection layered on only when the upcaster is
//! known; upcasters are pure (kernel Rust here, never module code on the
//! reconstruction path); unknown kinds stay opaque-but-inspectable; missing
//! required objects are precise partial availability, never fabricated.

use std::collections::BTreeMap;
use std::path::Path;

use kb_s3_appendlog::{for_each_frame, LogWriter, Profile};
use serde_json::{json, Value};

pub const ENVELOPE_SCHEMA: u32 = 1;

// ---------- envelope ----------

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct Envelope {
    pub env: u32,
    pub seq: u64,
    pub evt: String,
    pub kind: String,
    #[serde(rename = "schema")]
    pub payload_schema: u32,
    pub payload: Value,
    #[serde(default)]
    pub refs: Vec<String>,
}

impl Envelope {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}

// ---------- upcaster registry ----------

pub type Upcaster = fn(&Value) -> Result<Value, String>;

/// kind -> (payload schema, upcaster to the latest interpretation)
pub struct Registry {
    upcasters: BTreeMap<(String, u32), Upcaster>,
}

impl Registry {
    pub fn new() -> Self {
        let mut upcasters = BTreeMap::new();
        upcasters.insert(("user_message".to_string(), 1), upcast_user_message_v1_to_v2 as Upcaster);
        upcasters.insert(("tool_result".to_string(), 1), upcast_tool_result_v1_to_v2 as Upcaster);
        Self { upcasters }
    }

    /// None = unknown kind/schema: the event stays opaque-but-inspectable.
    pub fn upcast(&self, kind: &str, schema: u32, payload: &Value) -> Result<Option<Value>, String> {
        match self.upcasters.get(&(kind.to_string(), schema)) {
            Some(f) => f(payload).map(Some),
            None => Ok(None),
        }
    }
}

/// Fixture upcaster: v1 user_message {text} -> v2 {text, role:"user"}.
fn upcast_user_message_v1_to_v2(p: &Value) -> Result<Value, String> {
    let text = p.get("text").and_then(|t| t.as_str()).ok_or("user_message v1: missing text")?;
    Ok(json!({ "text": text, "role": "user" }))
}

/// Fixture upcaster: v1 tool_result {tool, ok} -> v2 {tool, ok, summary}.
fn upcast_tool_result_v1_to_v2(p: &Value) -> Result<Value, String> {
    let tool = p.get("tool").and_then(|t| t.as_str()).ok_or("tool_result v1: missing tool")?;
    let ok = p.get("ok").and_then(|o| o.as_bool()).ok_or("tool_result v1: missing ok")?;
    Ok(json!({ "tool": tool, "ok": ok, "summary": if ok { "ok" } else { "failed" } }))
}

// ---------- reconstruction ----------

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

pub fn reconstruct(path: &Path, registry: &Registry, objects: &std::collections::HashSet<String>) -> Result<Report, String> {
    let mut rep = Report::default();
    for_each_frame(path, |frame| {
        for line in frame.events {
            let env: Envelope = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::other(format!("envelope: {e}")))?;
            if env.env != ENVELOPE_SCHEMA {
                return Err(std::io::Error::other(format!("envelope schema {} != {ENVELOPE_SCHEMA}", env.env)));
            }
            rep.events += 1;
            let stat = rep.kinds.entry(env.kind.clone()).or_default();
            stat.schema = env.payload_schema;
            stat.count += 1;
            match registry.upcast(&env.kind, env.payload_schema, &env.payload) {
                Ok(Some(_)) => stat.upcasted += 1,
                Ok(None) => {
                    stat.opaque += 1;
                    stat.opaque_reason.get_or_insert_with(|| format!("no upcaster for kind '{}' schema {}", env.kind, env.payload_schema));
                }
                Err(e) => {
                    stat.opaque += 1;
                    stat.opaque_reason.get_or_insert_with(|| e.clone());
                    rep.upcast_errors.push(format!("{}@{}: {e}", env.kind, env.seq));
                }
            }
            for r in env.refs {
                if !objects.contains(&r) {
                    rep.missing_objects.push(r);
                }
            }
        }
        Ok(())
    })
    .map_err(|e| format!("recover: {e}"))?;
    Ok(rep)
}

// ---------- fixture ----------

pub fn write_fixture(path: &Path) -> std::io::Result<()> {
    let mut w = LogWriter::open(path, "demo")?;
    let events = [
        Envelope { env: 1, seq: 0, evt: "e1".into(), kind: "user_message".into(), payload_schema: 1, payload: json!({"text": "hello"}), refs: vec![] },
        Envelope { env: 1, seq: 1, evt: "e2".into(), kind: "tool_result".into(), payload_schema: 1, payload: json!({"tool": "read_file", "ok": true}), refs: vec!["blake3:aaaa".into()] },
        // references an object that will NOT exist: partial availability
        Envelope { env: 1, seq: 2, evt: "e3".into(), kind: "tool_result".into(), payload_schema: 1, payload: json!({"tool": "read_file", "ok": false}), refs: vec!["blake3:deadbeef".into()] },
        // unknown kind/schema: opaque-but-inspectable
        Envelope { env: 1, seq: 3, evt: "e4".into(), kind: "future_kind".into(), payload_schema: 9, payload: json!({"mystery": 42}), refs: vec![] },
    ];
    let batch: Vec<String> = events.iter().map(|e| e.to_line()).collect();
    w.append_frame(&batch, Profile::Fast)?;
    // an object store with one of the two referenced objects present
    std::fs::create_dir_all(path.parent().unwrap().join("objects"))?;
    std::fs::write(path.parent().unwrap().join("objects/blake3:aaaa"), b"object-bytes")?;
    Ok(())
}
