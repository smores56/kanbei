//! Provider gateway (R-19 tier 2 native built-in): one provider engine,
//! normalized lifecycle, model-call records, egress entries, and credential
//! custody (R-28/D-06: key injected at call time only, never canonical).

use std::fmt;
use std::time::Duration;

use kanbei_core::digest::Digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

// ---------- config ----------
/// Where the API key lives. Credentials are kernel-held and injected into
/// requests at call time only; they never enter canonical records, snapshots,
/// or objects (R-28/D-06). OS-keychain custody is deferred — the env source
/// is the MVP default and the seam is this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// Read the key from this environment variable at call time.
    Env(String),
    /// Inline key from config — never serialized into records.
    Inline(String),
}

/// One normalized provider configuration.
#[derive(Clone)]
pub struct ProviderConfig {
    /// Provider identity recorded in egress entries.
    pub provider: String,
    /// Wire model name.
    pub model: String,
    /// OpenAI-compatible base URL (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
    pub key: KeySource,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Duration,
}

// ---------- wire types ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool role: the tool_call_id this result answers.
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Canonicalized arguments (deterministic key ordering).
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// Canonical tool schemas (deterministic ordering).
    pub tools: Vec<Value>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
}

// ---------- engine seam ----------

/// One provider engine. The gateway owns the single wire protocol; the seam
/// keeps the gate deterministic (FakeEngine) without an HTTP round trip.
pub trait ProviderEngine: Send + Sync {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ProviderError>;
    /// Provider identity recorded in egress entries.
    fn identity(&self) -> &str;
    fn as_any(&self) -> &dyn std::any::Any;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider {provider}: {message}")]
    Transport { provider: String, message: String },
    #[error("provider {provider}: HTTP {status} {body}")]
    Http { provider: String, status: u16, body: String },
    #[error("provider {provider}: malformed response {message}")]
    Malformed { provider: String, message: String },
    #[error("provider {provider}: missing API key (source {name})")]
    MissingKey { provider: String, name: String },
    #[error("provider {provider}: request rejected {message}")]
    Rejected { provider: String, message: String },
    #[error("provider {provider}: timed out after {secs}s")]
    Timeout { provider: String, secs: u64 },
}

/// Resolve the configured key at call time — the only place credentials are
/// materialized (R-28/D-06).
pub fn resolve_key(cfg: &ProviderConfig) -> Result<String, ProviderError> {
    match &cfg.key {
        KeySource::Env(name) => std::env::var(name).map_err(|_| ProviderError::MissingKey {
            provider: cfg.provider.clone(),
            name: name.clone(),
        }),
        KeySource::Inline(key) => Ok(key.clone()),
    }
}

// ---------- OpenAI-compatible HTTP engine ----------

/// The one real engine: OpenAI-compatible `/chat/completions` over HTTPS.
pub struct HttpEngine {
    cfg: ProviderConfig,
}

impl HttpEngine {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self { cfg }
    }
}

impl ProviderEngine for HttpEngine {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let key = resolve_key(&self.cfg)?;
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": req.model,
            "messages": req.messages.iter().map(|m| {
                let mut obj = serde_json::Map::new();
                obj.insert("role".into(), json!(match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                }));
                obj.insert("content".into(), json!(m.content));
                if let Some(id) = &m.tool_call_id {
                    obj.insert("tool_call_id".into(), json!(id));
                }
                Value::Object(obj)
            }).collect::<Vec<_>>(),
        });
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(self.cfg.timeout))
                .build(),
        );
        let mut resp =
            match agent.post(&url).header("Authorization", &format!("Bearer {key}")).send_json(&body) {
                Ok(r) => r,
                Err(ureq::Error::Timeout(_)) => {
                    return Err(ProviderError::Timeout {
                        provider: self.cfg.provider.clone(),
                        secs: self.cfg.timeout.as_secs(),
                    })
                }
                Err(ureq::Error::StatusCode(code)) => {
                    return Err(ProviderError::Http {
                        provider: self.cfg.provider.clone(),
                        status: code,
                        body: String::new(),
                    })
                }
                Err(e) => {
                    return Err(ProviderError::Transport {
                        provider: self.cfg.provider.clone(),
                        message: e.to_string(),
                    })
                }
            };
        let status = resp.status();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| ProviderError::Malformed {
                provider: self.cfg.provider.clone(),
                message: e.to_string(),
            })?;
        if status != 200 {
            return Err(ProviderError::Http {
                provider: self.cfg.provider.clone(),
                status: status.into(),
                body: text,
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| ProviderError::Malformed {
            provider: self.cfg.provider.clone(),
            message: e.to_string(),
        })?;
        parse_openai_response(&self.cfg.provider, v)
    }

    fn identity(&self) -> &str {
        &self.cfg.provider
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Parse an OpenAI-compatible chat completion body into the canonical
/// response shape. Accepts both the `tool_calls` and the plain `content`
/// shapes; tool-call arguments arrive as a JSON string and are re-parsed to
/// canonical values.
pub fn parse_openai_response(
    provider: &str,
    v: Value,
) -> Result<CompletionResponse, ProviderError> {
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| ProviderError::Malformed {
            provider: provider.into(),
            message: "missing choices[0]".into(),
        })?;
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let tool_calls = choice
        .get("message")
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = tc.pointer("/function/name")?.as_str()?.to_string();
                    let args = tc
                        .pointer("/function/arguments")
                        .and_then(|a| a.as_str())
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null);
                    Some(ToolCall { id, name, arguments: args })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let finish = match choice.get("finish_reason").and_then(|f| f.as_str()) {
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    };
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
        output_tokens: u.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
    });
    let usage = usage.ok_or_else(|| ProviderError::Malformed {
        provider: provider.into(),
        message: "missing usage".into(),
    })?;
    Ok(CompletionResponse { content, tool_calls, finish_reason: finish, usage })
}

// ---------- deterministic fake engine (gate/tests) ----------

/// Scripted deterministic engine for the gate: returns the next queued
/// response, records every request for assertions. No network.
pub struct FakeEngine {
    cfg: ProviderConfig,
    responses: std::sync::Mutex<std::collections::VecDeque<CompletionResponse>>,
    pub requests: std::sync::Mutex<Vec<CompletionRequest>>,
}

impl FakeEngine {
    pub fn new(cfg: ProviderConfig, responses: Vec<CompletionResponse>) -> Self {
        Self {
            cfg,
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn push(&self, r: CompletionResponse) {
        self.responses.lock().unwrap().push_back(r);
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl ProviderEngine for FakeEngine {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        let mut q = self.responses.lock().unwrap();
        q.pop_front().ok_or_else(|| ProviderError::Rejected {
            provider: self.cfg.provider.clone(),
            message: "no scripted response left".into(),
        })
    }

    fn identity(&self) -> &str {
        &self.cfg.provider
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ---------- canonical records ----------

/// Model-call cache plan: M3 uses no provider-side caching (the cache-aware
/// projection lands in M4); the field exists so records stay schema-stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachePlan {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheOutcome {
    Miss,
}

/// The canonical `model_call` event payload (architecture.md:141): what
/// influenced the call and what it cost. Recorded by the session at intent
/// commit; the outcome event repeats the rendered digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCallRecord {
    /// Provider + model identity.
    pub provider: String,
    pub model: String,
    /// Projection/module hashes that produced the context (M4 fills the
    /// staged pipeline; M3 records the rendered context hash only).
    pub projection_hashes: Vec<Digest>,
    pub module_hashes: Vec<Digest>,
    /// Selected event IDs/ranges fed to the renderer.
    pub selected_events: Vec<u64>,
    /// Hash of the rendered provider context.
    pub rendered_hash: Digest,
    /// Model/provider parameters (temperature, max_tokens).
    pub params: Value,
    pub cache_plan: CachePlan,
    pub cache_outcome: CacheOutcome,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub finish_reason: FinishReason,
}

/// Canonical egress entry (R-15): one per model call. Diagnostics and
/// policy may read these; raw provider bytes never enter canonical records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressEntry {
    pub provider: String,
    /// Sensitivity classes egressed in this call (M3: the single "call"
    /// class; per-fragment classes arrive with the M4 projection).
    pub sensitivity_classes: Vec<String>,
    /// Origin snapshot digest of the call.
    pub origin_snapshot: Option<Digest>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Minimal context renderer (M3): renders a bounded slice of envelope
/// payloads into a stable provider context and hashes the result. The full
/// typed staged pipeline (TrajectoryView … ValidProviderContext) is M4; this
/// is the M3 seam with the same record contract.
pub fn render_context(
    events: &[(u64, String, &Value)],
    max_chars: usize,
) -> Result<(String, Digest), String> {
    let mut out = String::new();
    for (seq, kind, payload) in events {
        let line = serde_json::to_string(&json!({ "seq": seq, "kind": kind, "payload": payload }))
            .map_err(|e| format!("render: {e}"))?;
        if !out.is_empty() {
            out.push('\n');
        }
        if out.len() + line.len() > max_chars {
            break;
        }
        out.push_str(&line);
    }
    let digest = Digest::new(out.as_bytes());
    Ok((out, digest))
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("key", &match &self.key {
                KeySource::Env(name) => format!("env:{name}"),
                KeySource::Inline(_) => "inline:redacted".to_string(),
            })
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ProviderConfig {
        ProviderConfig {
            provider: "fake".into(),
            model: "test-model".into(),
            base_url: "http://localhost:0/v1".into(),
            key: KeySource::Env("KANBEI_TEST_KEY".into()),
            temperature: None,
            max_tokens: Some(100),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn key_resolution_inline_never_leaks() {
        let c = ProviderConfig {
            key: KeySource::Inline("sekrit".into()),
            ..cfg()
        };
        assert_eq!(resolve_key(&c).unwrap(), "sekrit");
        // Debug never prints the key.
        assert!(!format!("{c:?}").contains("sekrit"));
    }

    #[test]
    fn key_resolution_env() {
        unsafe {
            std::env::set_var("KANBEI_TEST_KEY", "k");
        }
        let c = cfg();
        assert_eq!(resolve_key(&c).unwrap(), "k");
    }

    #[test]
    fn parse_openai_content_response() {
        let v = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3 }
        });
        let r = parse_openai_response("fake", v).unwrap();
        assert_eq!(r.content.as_deref(), Some("hello"));
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.usage, Usage { input_tokens: 10, output_tokens: 3 });
        assert_eq!(r.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn parse_openai_tool_calls_response() {
        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "fs_read", "arguments": "{\"path\":\"/a\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 7 }
        });
        let r = parse_openai_response("fake", v).unwrap();
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "fs_read");
        assert_eq!(r.tool_calls[0].arguments, json!({"path": "/a"}));
        assert_eq!(r.finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn malformed_response_is_explicit() {
        let err = parse_openai_response("fake", json!({})).unwrap_err();
        assert!(matches!(err, ProviderError::Malformed { .. }));
    }

    #[test]
    fn fake_engine_scripted_and_records() {
        let engine = FakeEngine::new(
            cfg(),
            vec![CompletionResponse {
                content: Some("hi".into()),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                usage: Usage { input_tokens: 1, output_tokens: 1 },
            }],
        );
        let req = CompletionRequest {
            model: "test-model".into(),
            messages: vec![Message { role: Role::User, content: "x".into(), tool_call_id: None }],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        };
        let r = engine.complete(&req).unwrap();
        assert_eq!(r.content.as_deref(), Some("hi"));
        assert_eq!(engine.request_count(), 1);
        assert_eq!(engine.identity(), "fake");
    }

    #[test]
    fn render_context_bounded_and_deterministic() {
        let payload = json!({"text": "hello"});
        let (s1, d1) = render_context(&[(0u64, "user_message".into(), &payload)], 1000).unwrap();
        let (s2, d2) = render_context(&[(0u64, "user_message".into(), &payload)], 1000).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(d1, d2);
        let (short, _) =
            render_context(&[(0u64, "a".into(), &payload), (1u64, "b".into(), &payload)], 60)
                .unwrap();
        assert!(short.contains("\"kind\":\"a\""));
        assert!(!short.contains("\"kind\":\"b\""));
    }

    #[test]
    fn record_roundtrip() {
        let rec = ModelCallRecord {
            provider: "fake".into(),
            model: "m".into(),
            projection_hashes: vec![],
            module_hashes: vec![],
            selected_events: vec![1, 2, 3],
            rendered_hash: Digest::new(b"ctx"),
            params: json!({"temperature": 0.2}),
            cache_plan: CachePlan::None,
            cache_outcome: CacheOutcome::Miss,
            input_tokens: 5,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: ModelCallRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
        let e = EgressEntry {
            provider: "fake".into(),
            sensitivity_classes: vec!["call".into()],
            origin_snapshot: Some(Digest::new(b"s")),
            input_tokens: 5,
            output_tokens: 2,
        };
        let back: EgressEntry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
