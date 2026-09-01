//! Provider gateway (R-19 tier 2 native built-in): two provider wire
//! protocols (OpenAI-compatible Chat Completions + Anthropic Messages API,
//! M9 wave 3), normalized lifecycle, model-call records, egress entries, and
//! credential custody (R-28/D-06: key injected at call time only, never
//! canonical).

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

impl ProviderConfig {
    /// Canonical content-addressed bytes for the execution-snapshot
    /// manifest's `provider_config` pin: provider/model/base-url plus a
    /// key-source fingerprint — the key itself is never serialized (the
    /// egress redaction rules, R-15/R-28). The bytes are installed as an
    /// object before the manifest is pinned (closure-valid).
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let key_source = match &self.key {
            KeySource::Env(name) => format!("env:{name}"),
            KeySource::Inline(_) => "inline:redacted".to_string(),
        };
        serde_json::to_vec(&json!({
            "provider": self.provider,
            "model": self.model,
            "base_url": self.base_url,
            "key_source": key_source,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "timeout_secs": self.timeout.as_secs(),
        }))
        .expect("provider config serialization cannot fail")
    }
}

/// One normalized provider configuration.
#[derive(Clone)]
pub struct ProviderConfig {
    /// Provider identity recorded in egress entries.
    pub provider: String,
    /// Wire model name.
    pub model: String,
    /// Provider base URL. The OpenAI-compatible protocol appends
    /// `/chat/completions` (e.g. `https://api.openai.com/v1`); the Anthropic
    /// protocol treats this as the API root and appends `/v1/messages`
    /// (e.g. `https://api.anthropic.com`).
    pub base_url: String,
    pub key: KeySource,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Duration,
}

/// Provider wire protocol (M9 wave 3): which vendor API shape the engine
/// speaks. Serialized lowercase; configs without the field default to the
/// OpenAI-compatible shape (backward compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireProtocol {
    /// OpenAI-compatible `/chat/completions` ([`HttpEngine`]) — the default.
    #[serde(alias = "openai")]
    OpenAI,
    /// Anthropic Messages API `/v1/messages` ([`AnthropicEngine`]).
    #[serde(alias = "anthropic")]
    Anthropic,
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
    /// Assistant tool calls from previous turns, in conversation order. The
    /// OpenAI engine ignores them; the Anthropic engine replays them as
    /// `tool_use` blocks on the last assistant turn (the API requires
    /// `tool_use` before `tool_result`). M9 wave 3.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// R-18/E-07: opaque reasoning artifacts replayed from the previous
    /// same-provider call (base64, verbatim; never sent cross-provider).
    #[serde(default)]
    pub opaque_artifacts: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    /// R-18/E-07: the model's flag that its reasoning does not follow from
    /// the projection (free-form reason, e.g. "projection").
    #[serde(default)]
    pub discontinuity: Option<String>,
    /// R-18/E-07: opaque reasoning artifact bytes (base64). Stored verbatim;
    /// the kernel never interprets them.
    #[serde(default)]
    pub opaque_artifacts: Option<String>,
}

// ---------- engine seam ----------

/// One provider engine. The gateway owns the wire protocols
/// (OpenAI-compatible Chat Completions + Anthropic Messages API, M9 wave 3);
/// the seam keeps the gate deterministic (FakeEngine) without an HTTP round
/// trip.
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
    Http {
        provider: String,
        status: u16,
        body: String,
    },
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

/// The OpenAI-compatible engine: `/chat/completions` over HTTPS (the default
/// wire protocol).
pub struct HttpEngine {
    cfg: ProviderConfig,
}

impl HttpEngine {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self { cfg }
    }
}

/// Build the OpenAI-compatible request body from the canonical request.
/// Every system message folds into ONE leading system message (joined with
/// "\n\n") — the OpenAI API tolerates many system turns, but provider chat
/// templates (llama.cpp Jinja, e.g. Qwen's "System message must be at the
/// beginning") reject more than one; mirrors the Anthropic body's `system`
/// fold. Canonical tool schemas map to the OpenAI `tools` shape.
fn openai_body(cfg: &ProviderConfig, req: &CompletionRequest) -> Result<Value, ProviderError> {
    let systems: Vec<&str> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    let messages: Vec<Value> = {
        let mut out = Vec::new();
        if !systems.is_empty() {
            out.push(json!({"role": "system", "content": systems.join("\n\n")}));
        }
        for m in &req.messages {
            if m.role == Role::System {
                continue;
            }
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), json!(match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
                Role::System => "system",
            }));
            obj.insert("content".into(), json!(m.content));
            if let Some(id) = &m.tool_call_id {
                obj.insert("tool_call_id".into(), json!(id));
            }
            out.push(Value::Object(obj));
        }
        out
    };
    let mut body = json!({
        "model": req.model,
        "messages": messages,
    });
    if !req.tools.is_empty() {
        body["tools"] = json!(openai_tools(&cfg.provider, &req.tools)?);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    Ok(body)
}

impl ProviderEngine for HttpEngine {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let key = resolve_key(&self.cfg)?;
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let body = openai_body(&self.cfg, req)?;
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(self.cfg.timeout))
                .build(),
        );
        let mut resp = match agent
            .post(&url)
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(&body)
        {
            Ok(r) => r,
            Err(ureq::Error::Timeout(_)) => {
                return Err(ProviderError::Timeout {
                    provider: self.cfg.provider.clone(),
                    secs: self.cfg.timeout.as_secs(),
                });
            }
            Err(ureq::Error::StatusCode(code)) => {
                return Err(ProviderError::Http {
                    provider: self.cfg.provider.clone(),
                    status: code,
                    body: String::new(),
                });
            }
            Err(e) => {
                return Err(ProviderError::Transport {
                    provider: self.cfg.provider.clone(),
                    message: e.to_string(),
                });
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
                    Some(ToolCall {
                        id,
                        name,
                        arguments: args,
                    })
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
        output_tokens: u
            .get("completion_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(0),
    });
    let usage = usage.ok_or_else(|| ProviderError::Malformed {
        provider: provider.into(),
        message: "missing usage".into(),
    })?;
    Ok(CompletionResponse {
        content,
        tool_calls,
        finish_reason: finish,
        usage,
        // The OpenAI wire format carries no discontinuity/artifact fields
        // (R-18/E-07: opaque artifacts are provider-specific extensions).
        discontinuity: None,
        opaque_artifacts: None,
    })
}

// ---------- Anthropic Messages API engine (M9 wave 3) ----------

/// The Anthropic Messages API engine (M9 wave 3): a second provider wire
/// protocol behind the same [`ProviderEngine`] seam (architecture.md:700).
/// `base_url` is the API root (e.g. `https://api.anthropic.com`), NOT a
/// `/v1` path — this engine appends `/v1/messages`.
pub struct AnthropicEngine {
    cfg: ProviderConfig,
}

impl AnthropicEngine {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self { cfg }
    }
}

/// The Messages API requires `max_tokens`; the fallback chain is
/// request → config → this default.
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 1024;

impl ProviderEngine for AnthropicEngine {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let key = resolve_key(&self.cfg)?;
        let url = format!("{}/v1/messages", self.cfg.base_url.trim_end_matches('/'));
        let body = anthropic_body(&self.cfg, req)?;
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(self.cfg.timeout))
                .build(),
        );
        let mut resp = match agent
            .post(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send_json(&body)
        {
            Ok(r) => r,
            Err(ureq::Error::Timeout(_)) => {
                return Err(ProviderError::Timeout {
                    provider: self.cfg.provider.clone(),
                    secs: self.cfg.timeout.as_secs(),
                });
            }
            Err(ureq::Error::StatusCode(code)) => {
                return Err(ProviderError::Http {
                    provider: self.cfg.provider.clone(),
                    status: code,
                    body: String::new(),
                });
            }
            Err(e) => {
                return Err(ProviderError::Transport {
                    provider: self.cfg.provider.clone(),
                    message: e.to_string(),
                });
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
        parse_anthropic_response(&self.cfg.provider, v)
    }

    fn identity(&self) -> &str {
        &self.cfg.provider
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Build the Messages API request body from the canonical request. System
/// messages fold into the top-level `system` field (joined with "\n\n");
/// tool results fold into user turns (`tool_result` blocks — Anthropic has
/// no separate tool role); the request's tool calls replay as `tool_use`
/// blocks on the last assistant turn (the API requires `tool_use` before
/// `tool_result`). Turn order is preserved.
fn anthropic_body(
    cfg: &ProviderConfig,
    req: &CompletionRequest,
) -> Result<Value, ProviderError> {
    let max_tokens = req
        .max_tokens
        .or(cfg.max_tokens)
        .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
    let system = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut body = serde_json::Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(max_tokens));
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !system.is_empty() {
        body.insert("system".into(), json!(system));
    }
    let mut messages: Vec<Value> = Vec::new();
    let mut last_assistant: Option<usize> = None;
    for m in req.messages.iter().filter(|m| m.role != Role::System) {
        match m.role {
            Role::Assistant => {
                last_assistant = Some(messages.len());
                messages.push(json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": m.content}],
                }));
            }
            Role::Tool => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id,
                        "content": m.content,
                    }],
                }));
            }
            _ => {
                messages.push(json!({
                    "role": "user",
                    "content": if m.tool_call_id.is_some() {
                        Value::Array(vec![json!({"type": "text", "text": m.content})])
                    } else {
                        json!(m.content)
                    },
                }));
            }
        }
    }
    if !req.tool_calls.is_empty()
        && let Some(i) = last_assistant
    {
        let tool_uses = req
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.arguments,
                })
            })
            .collect::<Vec<_>>();
        if let Some(arr) = messages[i].get_mut("content").and_then(|c| c.as_array_mut()) {
            arr.extend(tool_uses);
        }
    }
    body.insert("messages".into(), json!(messages));
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            json!(anthropic_tools(&cfg.provider, &req.tools)?),
        );
    }
    Ok(Value::Object(body))
}

/// Map the canonical tool schemas (`{name, description, input, output}` —
/// `ToolRegistry::canonical_json`, R-05) to the OpenAI `tools` shape: each
/// becomes `{"type":"function","function":{name,description,parameters}}`,
/// where `parameters` carries the `input` schema (the `output` schema is
/// record-only, not part of the OpenAI wire contract). Unknown shapes are
/// Malformed with the tool index.
fn openai_tools(provider: &str, tools: &[Value]) -> Result<Vec<Value>, ProviderError> {
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let name = t.get("name").and_then(|n| n.as_str()).ok_or_else(|| {
                ProviderError::Malformed {
                    provider: provider.into(),
                    message: format!("tools[{i}]: missing name"),
                }
            })?;
            let mut function = serde_json::Map::new();
            function.insert("name".into(), json!(name));
            if let Some(d) = t.get("description").and_then(|d| d.as_str()) {
                function.insert("description".into(), json!(d));
            }
            function.insert(
                "parameters".into(),
                t.get("input").cloned().unwrap_or(Value::Null),
            );
            let mut out = serde_json::Map::new();
            out.insert("type".into(), json!("function"));
            out.insert("function".into(), Value::Object(function));
            Ok(Value::Object(out))
        })
        .collect()
}

/// Map the canonical tool schemas to the Anthropic `tools` shape: each
/// `{name, description, input, output}` becomes
/// `{name, description, input_schema: input}`. Unknown shapes are Malformed
/// with the tool index.
fn anthropic_tools(provider: &str, tools: &[Value]) -> Result<Vec<Value>, ProviderError> {
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let name = t.get("name").and_then(|n| n.as_str()).ok_or_else(|| {
                ProviderError::Malformed {
                    provider: provider.into(),
                    message: format!("tools[{i}]: missing name"),
                }
            })?;
            let mut out = serde_json::Map::new();
            out.insert("name".into(), json!(name));
            if let Some(d) = t.get("description").and_then(|d| d.as_str()) {
                out.insert("description".into(), json!(d));
            }
            out.insert(
                "input_schema".into(),
                t.get("input").cloned().unwrap_or(Value::Null),
            );
            Ok(Value::Object(out))
        })
        .collect()
}

/// Parse an Anthropic Messages API body into the canonical response shape.
/// Text blocks join with "\n" in block order; `tool_use` blocks map 1:1 to
/// tool calls (Anthropic `input` arrives as a JSON object, unlike OpenAI's
/// string arguments). `stop_reason` maps `tool_use` → ToolCalls and
/// `max_tokens` → Length; Anthropic has no content-filter equivalent.
pub fn parse_anthropic_response(
    provider: &str,
    v: Value,
) -> Result<CompletionResponse, ProviderError> {
    let blocks = v.get("content").ok_or_else(|| ProviderError::Malformed {
        provider: provider.into(),
        message: "missing content".into(),
    })?;
    let blocks = blocks.as_array().ok_or_else(|| ProviderError::Malformed {
        provider: provider.into(),
        message: "content is not an array".into(),
    })?;
    let text = blocks
        .iter()
        .filter_map(|b| {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                b.get("text").and_then(|t| t.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let content = if text.is_empty() { None } else { Some(text) };
    let tool_calls = blocks
        .iter()
        .filter_map(|b| {
            if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                return None;
            }
            let id = b.get("id")?.as_str()?.to_string();
            let name = b.get("name")?.as_str()?.to_string();
            let arguments = b.get("input").cloned().unwrap_or(Value::Null);
            Some(ToolCall {
                id,
                name,
                arguments,
            })
        })
        .collect::<Vec<_>>();
    let finish = match v.get("stop_reason").and_then(|s| s.as_str()) {
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::Length,
        _ => FinishReason::Stop,
    };
    let usage = v.get("usage").ok_or_else(|| ProviderError::Malformed {
        provider: provider.into(),
        message: "missing usage".into(),
    })?;
    Ok(CompletionResponse {
        content,
        tool_calls,
        finish_reason: finish,
        usage: Usage {
            input_tokens: usage
                .get("input_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0),
            output_tokens: usage
                .get("output_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0),
        },
        // The Anthropic wire format carries no discontinuity/artifact fields
        // (R-18/E-07: opaque artifacts are provider-specific extensions).
        discontinuity: None,
        opaque_artifacts: None,
    })
}

/// Build the engine for a protocol: OpenAI → [`HttpEngine`], Anthropic →
/// [`AnthropicEngine`]. The session calls this when no engine was injected
/// (M9 wave 3: `SessionConfig.protocol` drives the choice).
pub fn engine_for(cfg: &ProviderConfig, protocol: WireProtocol) -> Box<dyn ProviderEngine> {
    match protocol {
        WireProtocol::OpenAI => Box::new(HttpEngine::new(cfg.clone())),
        WireProtocol::Anthropic => Box::new(AnthropicEngine::new(cfg.clone())),
    }
}

// ---------- deterministic fake engine (gate/tests) ----------

/// Scripted deterministic engine for the gate: returns the next queued
/// response, records every request for assertions. No network. Scripted
/// responses may carry `discontinuity`/`opaque_artifacts` (R-18/E-07); the
/// request log captures what each call received (`requests[i].opaque_artifacts`).
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

/// Model-call cache plan: whether the provider may serve this call from a
/// cache and, if so, under which stable-prefix digest (architecture.md:141).
/// M3 records used `None`; M4 projection lowering (kanbei-context `lower`)
/// selects `StablePrefix`. Serialized lowercase; the old PascalCase variant
/// names still deserialize (backward compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CachePlan {
    /// No provider-side caching for this call.
    #[serde(alias = "None")]
    None,
    /// Serve the cached stable prefix when the digest matches (the lowering
    /// digest over the prefix fragments' ids, content hashes, dep hashes).
    StablePrefix { digest: Digest },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheOutcome {
    #[serde(alias = "Miss")]
    Miss,
    /// The provider served the cached stable prefix.
    Hit,
    /// A previously cached prefix was invalidated (e.g. stable-segment
    /// promotion); the reason is recorded.
    Invalidated { reason: String },
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
    /// Pinned project/lifetime memory-root digests at call time (R-11: model
    /// calls pin exact memory roots); [lifetime, project], empty when
    /// unpinned.
    #[serde(default)]
    pub memory_roots: Vec<Digest>,
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
            .field(
                "key",
                &match &self.key {
                    KeySource::Env(name) => format!("env:{name}"),
                    KeySource::Inline(_) => "inline:redacted".to_string(),
                },
            )
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
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 3
            }
        );
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
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                },
                discontinuity: None,
                opaque_artifacts: None,
            }],
        );
        let req = CompletionRequest {
            model: "test-model".into(),
            messages: vec![Message {
                role: Role::User,
                content: "x".into(),
                tool_call_id: None,
            }],
            tools: vec![],
            temperature: None,
            max_tokens: None,
            tool_calls: vec![],
            opaque_artifacts: None,
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
        let (short, _) = render_context(
            &[(0u64, "a".into(), &payload), (1u64, "b".into(), &payload)],
            60,
        )
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
            memory_roots: vec![Digest::new(b"lifetime"), Digest::new(b"project")],
            input_tokens: 5,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: ModelCallRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
        // Pre-M4 records have no memory_roots field; serde(default) → empty.
        let old = r#"{"provider":"fake","model":"m","projection_hashes":[],"module_hashes":[],"selected_events":[],"rendered_hash":"blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262","params":{},"cache_plan":"None","cache_outcome":"Miss","input_tokens":0,"output_tokens":0,"finish_reason":"Stop"}"#;
        let back: ModelCallRecord = serde_json::from_str(old).unwrap();
        assert!(back.memory_roots.is_empty());
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

    #[test]
    fn cache_plan_outcome_variants_roundtrip_and_backward_compat() {
        let rec = ModelCallRecord {
            provider: "fake".into(),
            model: "m".into(),
            projection_hashes: vec![Digest::new(b"proj")],
            module_hashes: vec![],
            selected_events: vec![1, 2, 3],
            rendered_hash: Digest::new(b"ctx"),
            params: json!({"temperature": 0.2}),
            cache_plan: CachePlan::StablePrefix {
                digest: Digest::new(b"prefix"),
            },
            cache_outcome: CacheOutcome::Invalidated {
                reason: "promotion".into(),
            },
            memory_roots: vec![Digest::new(b"lifetime")],
            input_tokens: 5,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: ModelCallRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
        assert!(s.contains("\"stableprefix\""));
        assert!(s.contains("\"invalidated\""));
        // Pre-M4 records serialized the PascalCase variant names; they must
        // still deserialize unchanged.
        let old = r#"{"provider":"fake","model":"m","projection_hashes":[],"module_hashes":[],"selected_events":[],"rendered_hash":"blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262","params":{},"cache_plan":"None","cache_outcome":"Miss","input_tokens":0,"output_tokens":0,"finish_reason":"Stop"}"#;
        let back: ModelCallRecord = serde_json::from_str(old).unwrap();
        assert_eq!(back.cache_plan, CachePlan::None);
        assert_eq!(back.cache_outcome, CacheOutcome::Miss);
    }

    // ---------- M9 wave 3: Anthropic Messages API ----------

    #[test]
    fn wire_protocol_serde_lowercase() {
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"openai\"").unwrap(),
            WireProtocol::OpenAI
        );
        assert_eq!(
            serde_json::from_str::<WireProtocol>("\"anthropic\"").unwrap(),
            WireProtocol::Anthropic
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::Anthropic).unwrap(),
            "\"anthropic\""
        );
    }

    #[test]
    fn anthropic_body_system_folds_and_order_preserved() {
        let body = anthropic_body(
            &cfg(),
            &CompletionRequest {
                model: "claude".into(),
                messages: vec![
                    Message {
                        role: Role::System,
                        content: "sys1".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::User,
                        content: "u1".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::System,
                        content: "sys2".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::User,
                        content: "u2".into(),
                        tool_call_id: None,
                    },
                ],
                tools: vec![],
                temperature: Some(0.5),
                max_tokens: None,
                tool_calls: vec![],
                opaque_artifacts: None,
            },
        )
        .unwrap();
        assert_eq!(body["model"], "claude");
        assert_eq!(body["system"], "sys1\n\nsys2");
        assert_eq!(body["temperature"], 0.5);
        // System turns fold away; user turns keep their order.
        assert_eq!(
            body["messages"],
            json!([
                {"role": "user", "content": "u1"},
                {"role": "user", "content": "u2"},
            ])
        );
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn anthropic_body_tool_result_folding() {
        let body = anthropic_body(
            &cfg(),
            &CompletionRequest {
                model: "claude".into(),
                messages: vec![
                    Message {
                        role: Role::User,
                        content: "read /a".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Assistant,
                        content: "ok".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Tool,
                        content: "content-of-a".into(),
                        tool_call_id: Some("tc1".into()),
                    },
                ],
                tools: vec![],
                temperature: None,
                max_tokens: None,
                tool_calls: vec![],
                opaque_artifacts: None,
            },
        )
        .unwrap();
        assert_eq!(
            body["messages"][1],
            json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]})
        );
        assert_eq!(
            body["messages"][2],
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tc1",
                    "content": "content-of-a",
                }],
            })
        );
    }

    #[test]
    fn anthropic_body_assistant_tool_use_blocks() {
        let body = anthropic_body(
            &cfg(),
            &CompletionRequest {
                model: "claude".into(),
                messages: vec![
                    Message {
                        role: Role::Assistant,
                        content: "calling".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Tool,
                        content: "out".into(),
                        tool_call_id: Some("tc1".into()),
                    },
                ],
                tools: vec![],
                temperature: None,
                max_tokens: None,
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "fs_read".into(),
                    arguments: json!({"path": "/a"}),
                }],
                opaque_artifacts: None,
            },
        )
        .unwrap();
        assert_eq!(
            body["messages"][0],
            json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "tc1", "name": "fs_read", "input": {"path": "/a"}},
                ],
            })
        );
    }

    #[test]
    fn anthropic_body_tools_mapping_and_malformed() {
        let body = anthropic_body(
            &cfg(),
            &CompletionRequest {
                model: "claude".into(),
                messages: vec![],
                tools: vec![json!({
                    "name": "fs_read",
                    "description": "Read a file",
                    "input": {"type": "object"},
                    "output": {"type": "object"},
                })],
                temperature: None,
                max_tokens: None,
                tool_calls: vec![],
                opaque_artifacts: None,
            },
        )
        .unwrap();
        assert_eq!(
            body["tools"],
            json!([
                {"name": "fs_read", "description": "Read a file", "input_schema": {"type": "object"}},
            ])
        );
        let err = anthropic_body(
            &cfg(),
            &CompletionRequest {
                model: "claude".into(),
                messages: vec![],
                tools: vec![json!({"description": "no name"})],
                temperature: None,
                max_tokens: None,
                tool_calls: vec![],
                opaque_artifacts: None,
            },
        )
        .unwrap_err();
        match err {
            ProviderError::Malformed { message, .. } => {
                assert!(message.contains("tools[0]"));
            }
            _ => panic!("expected Malformed, got {err:?}"),
        }
    }

    #[test]
    fn openai_tools_mapping_and_malformed() {
        let tools = vec![json!({
            "name": "fs_read",
            "description": "Read a file",
            "input": {"type": "object"},
            "output": {"type": "object"},
        })];
        assert_eq!(
            openai_tools("http", &tools).unwrap(),
            vec![json!({
                "type": "function",
                "function": {
                    "name": "fs_read",
                    "description": "Read a file",
                    "parameters": {"type": "object"},
                },
            })]
        );
        let err = openai_tools("http", &[json!({"description": "no name"})]).unwrap_err();
        match err {
            ProviderError::Malformed { message, .. } => {
                assert!(message.contains("tools[0]"));
            }
            _ => panic!("expected Malformed, got {err:?}"),
        }
    }

    #[test]
    fn openai_body_folds_systems_and_maps_tools() {
        let body = openai_body(
            &cfg(),
            &CompletionRequest {
                model: "test-model".into(),
                messages: vec![
                    Message {
                        role: Role::System,
                        content: "sys1".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::User,
                        content: "u1".into(),
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::System,
                        content: "sys2".into(),
                        tool_call_id: None,
                    },
                ],
                tools: vec![json!({
                    "name": "fs_read",
                    "description": "Read a file",
                    "input": {"type": "object"},
                })],
                temperature: Some(0.5),
                max_tokens: None,
                tool_calls: vec![],
                opaque_artifacts: None,
            },
        )
        .unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["temperature"], 0.5);
        // Both system turns fold into one leading system message (provider
        // chat templates reject more than one); turn order is preserved.
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "sys1\n\nsys2"},
                {"role": "user", "content": "u1"},
            ])
        );
        assert_eq!(
            body["tools"],
            json!([
                {
                    "type": "function",
                    "function": {
                        "name": "fs_read",
                        "description": "Read a file",
                        "parameters": {"type": "object"},
                    },
                },
            ])
        );
    }

    #[test]
    fn anthropic_body_max_tokens_fallback_chain() {
        let req = |max_tokens| CompletionRequest {
            model: "claude".into(),
            messages: vec![],
            tools: vec![],
            temperature: None,
            max_tokens,
            tool_calls: vec![],
            opaque_artifacts: None,
        };
        let bare = ProviderConfig {
            max_tokens: None,
            ..cfg()
        };
        let configured = ProviderConfig {
            max_tokens: Some(500),
            ..cfg()
        };
        assert_eq!(anthropic_body(&bare, &req(None)).unwrap()["max_tokens"], 1024);
        assert_eq!(anthropic_body(&configured, &req(None)).unwrap()["max_tokens"], 500);
        assert_eq!(
            anthropic_body(&configured, &req(Some(900))).unwrap()["max_tokens"],
            900
        );
    }

    #[test]
    fn parse_anthropic_text_and_tool_use() {
        let v = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "tool_use", "id": "tc1", "name": "fs_read", "input": {"path": "/a"}},
                {"type": "text", "text": "second"},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 3},
        });
        let r = parse_anthropic_response("fake", v).unwrap();
        assert_eq!(r.content.as_deref(), Some("first\nsecond"));
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "tc1");
        assert_eq!(r.tool_calls[0].name, "fs_read");
        assert_eq!(r.tool_calls[0].arguments, json!({"path": "/a"}));
        assert_eq!(r.finish_reason, FinishReason::ToolCalls);
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 3
            }
        );
    }

    #[test]
    fn parse_anthropic_stop_reasons_and_usage() {
        let length = parse_anthropic_response(
            "fake",
            json!({
                "content": [{"type": "text", "text": "hi"}],
                "stop_reason": "max_tokens",
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }),
        )
        .unwrap();
        assert_eq!(length.finish_reason, FinishReason::Length);
        let stop = parse_anthropic_response(
            "fake",
            json!({
                "content": [{"type": "text", "text": "hi"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }),
        )
        .unwrap();
        assert_eq!(stop.finish_reason, FinishReason::Stop);
        assert_eq!(stop.content.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_anthropic_malformed() {
        let missing_content =
            parse_anthropic_response("fake", json!({"usage": {"input_tokens": 1}})).unwrap_err();
        assert!(
            matches!(missing_content, ProviderError::Malformed { ref message, .. } if message.contains("content"))
        );
        let missing_usage = parse_anthropic_response(
            "fake",
            json!({"content": [{"type": "text", "text": "hi"}]}),
        )
        .unwrap_err();
        assert!(
            matches!(missing_usage, ProviderError::Malformed { ref message, .. } if message.contains("usage"))
        );
    }

    #[test]
    fn anthropic_request_response_roundtrip() {
        let req = CompletionRequest {
            model: "claude".into(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: "sys".into(),
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: "read /a".into(),
                    tool_call_id: None,
                },
                Message {
                    role: Role::Assistant,
                    content: "calling".into(),
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "body-of-a".into(),
                    tool_call_id: Some("tc1".into()),
                },
            ],
            tools: vec![json!({
                "name": "fs_read",
                "description": "Read",
                "input": {"type": "object"},
            })],
            temperature: Some(0.2),
            max_tokens: None,
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "fs_read".into(),
                arguments: json!({"path": "/a"}),
            }],
            opaque_artifacts: None,
        };
        let body = anthropic_body(
            &ProviderConfig {
                max_tokens: None,
                ..cfg()
            },
            &req,
        )
        .unwrap();
        assert_eq!(body["model"], "claude");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(
            body["tools"],
            json!([
                {"name": "fs_read", "description": "Read", "input_schema": {"type": "object"}},
            ])
        );
        // The inverse: a response echoing the request's tool call parses back
        // to the same tool call list.
        let v = json!({
            "content": [
                {"type": "text", "text": "done"},
                {"type": "tool_use", "id": "tc1", "name": "fs_read", "input": {"path": "/a"}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 7, "output_tokens": 2},
        });
        let r = parse_anthropic_response("fake", v).unwrap();
        assert_eq!(r.content.as_deref(), Some("done"));
        assert_eq!(r.tool_calls, req.tool_calls);
        assert_eq!(r.finish_reason, FinishReason::ToolCalls);
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 7,
                output_tokens: 2
            }
        );
    }

    #[test]
    fn engine_for_selects_protocol() {
        let http = engine_for(&cfg(), WireProtocol::OpenAI);
        assert!(http.as_any().downcast_ref::<HttpEngine>().is_some());
        let anthropic = engine_for(&cfg(), WireProtocol::Anthropic);
        assert!(anthropic.as_any().downcast_ref::<AnthropicEngine>().is_some());
        assert_eq!(anthropic.identity(), "fake");
    }
}
