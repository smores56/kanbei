//! Typed tools (M3 agent spine): the deterministic tool registry, the tool
//! FSM records (ToolCallId/ToolIntent/ToolOutcome with origin_snapshot and
//! approval binding, R-16/D-11/D-12), and the native tool executors with
//! launch controls (R-28/D-S2: timeout, output limits, FD closure, tree
//! cancellation, default-deny environment).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kanbei_capabilities::{ApprovalIntent, Principal};
use kanbei_core::digest::Digest;
use kanbei_core::id::{BrandedId, Id128};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

// ---------- identities ----------

pub type ToolCallId = BrandedId;

pub fn tool_call_id() -> ToolCallId {
    BrandedId::new("call_", Id128::generate())
}

// ---------- tool registry (deterministic schemas) ----------

/// One typed tool schema. Serialization is canonical: the registry sorts by
/// name and schemas use sorted object keys (R-05/E-10 deterministic tool
/// schemas).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

impl ToolSchema {
    pub fn new(name: &str, description: &str, input: Value, output: Value) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: input,
            output_schema: output,
        }
    }
}

/// Canonical JSON shape of a schema (stable bytes for digests/records).
pub fn canonical_schema_json(s: &ToolSchema) -> Value {
    json!({
        "name": s.name,
        "description": s.description,
        "input": s.input_schema,
        "output": s.output_schema,
    })
}

/// The M3 built-in tool set (MVP tool list, architecture.md:603). Memory
/// tools are registered with explicit `Unavailable` dispatch until the M4
/// memory substrate exists — never silent.
pub fn builtin_tool_schemas() -> Vec<ToolSchema> {
    let mut v = vec![
        ToolSchema::new(
            "fs.read",
            "Read a file (bounded size).",
            json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
            json!({"type": "object", "properties": {"content": {"type": "string"}, "bytes": {"type": "integer"}}}),
        ),
        ToolSchema::new(
            "fs.search",
            "Search a directory tree for filenames/paths matching a substring.",
            json!({"type": "object", "required": ["root", "query"], "properties": {"root": {"type": "string"}, "query": {"type": "string"}, "max_results": {"type": "integer"}}}),
            json!({"type": "object", "properties": {"matches": {"type": "array", "items": {"type": "string"}}}}),
        ),
        ToolSchema::new(
            "fs.write",
            "Write a file (atomic replace; consequential — approval-gated).",
            json!({"type": "object", "required": ["path", "content"], "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}),
            json!({"type": "object", "properties": {"bytes": {"type": "integer"}}}),
        ),
        ToolSchema::new(
            "fs.patch",
            "Apply exact-string replacements to a file (consequential — approval-gated).",
            json!({"type": "object", "required": ["path", "replacements"], "properties": {"path": {"type": "string"}, "replacements": {"type": "array", "items": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}}}}}}),
            json!({"type": "object", "properties": {"applied": {"type": "integer"}}}),
        ),
        ToolSchema::new(
            "git.status",
            "Run `git status --porcelain` in a repository.",
            json!({"type": "object", "required": ["repo"], "properties": {"repo": {"type": "string"}}}),
            json!({"type": "object", "properties": {"output": {"type": "string"}}}),
        ),
        ToolSchema::new(
            "git.diff",
            "Run `git diff` in a repository (bounded output).",
            json!({"type": "object", "required": ["repo"], "properties": {"repo": {"type": "string"}, "max_output": {"type": "integer"}}}),
            json!({"type": "object", "properties": {"output": {"type": "string"}}}),
        ),
        ToolSchema::new(
            "process.exec",
            "Run a native subprocess with launch controls (consequential — approval-gated).",
            json!({"type": "object", "required": ["argv"], "properties": {"argv": {"type": "array", "items": {"type": "string"}}, "cwd": {"type": "string"}, "env": {"type": "object"}, "timeout_ms": {"type": "integer"}, "max_output": {"type": "integer"}}}),
            json!({"type": "object", "properties": {"exit": {"type": "integer"}, "stdout": {"type": "string"}, "stderr": {"type": "string"}, "timed_out": {"type": "boolean"}}}),
        ),
        ToolSchema::new(
            "todo.list",
            "List todo/task state entries.",
            json!({"type": "object", "properties": {}}),
            json!({"type": "object", "properties": {"items": {"type": "array"}}}),
        ),
        ToolSchema::new(
            "todo.update",
            "Update todo/task state (consequential — approval-gated).",
            json!({"type": "object", "required": ["key", "status"], "properties": {"key": {"type": "string"}, "status": {"type": "string"}}}),
            json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
        ),
        ToolSchema::new(
            "child.spawn",
            "Spawn a bounded child run (routes through the tool FSM — R-09).",
            json!({"type": "object", "required": ["goal"], "properties": {"goal": {"type": "string"}}}),
            json!({"type": "object", "properties": {"child_run": {"type": "string"}}}),
        ),
        ToolSchema::new(
            "memory.query",
            "Query the project memory claim DAG (unavailable until M4).",
            json!({"type": "object", "required": ["query"], "properties": {"query": {"type": "string"}}}),
            json!({"type": "object"}),
        ),
        ToolSchema::new(
            "memory.propose",
            "Propose a durable memory claim (unavailable until M4).",
            json!({"type": "object", "required": ["claim"], "properties": {"claim": {"type": "object"}}}),
            json!({"type": "object"}),
        ),
    ];
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Kernel-owned typed tool registry: deterministic names/schemas; lookup by
/// exact name. Modules contribute through the scope registries (M4+), not
/// here.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    schemas: BTreeMap<String, ToolSchema>,
}

impl ToolRegistry {
    pub fn builtin() -> Self {
        let mut r = Self::default();
        for s in builtin_tool_schemas() {
            r.schemas.insert(s.name.clone(), s);
        }
        r
    }

    pub fn with(mut self, schema: ToolSchema) -> Self {
        self.schemas.insert(schema.name.clone(), schema);
        self
    }

    pub fn get(&self, name: &str) -> Option<&ToolSchema> {
        self.schemas.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.schemas.keys().cloned().collect()
    }

    /// Canonical serialization for records/approvals: schema JSON sorted by
    /// name.
    pub fn canonical_json(&self) -> Value {
        let arr: Vec<Value> = self
            .schemas
            .values()
            .map(canonical_schema_json)
            .collect();
        json!(arr)
    }

    pub fn digest(&self) -> Digest {
        Digest::new(serde_json::to_string(&self.canonical_json()).unwrap_or_default().as_bytes())
    }
}

// ---------- tool FSM records ----------

/// Committed tool intent (committed before dispatch — B-05). Carries the
/// caller principal (R-14/D-02) and the origin snapshot (R-02/C-03).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolIntent {
    pub call_id: ToolCallId,
    pub run_id: Id128,
    pub principal: Principal,
    pub tool: String,
    /// Canonicalized arguments (object keys sorted recursively).
    pub args: Value,
    /// Approved approval-intent digest, if the tool was approved.
    pub approval: Option<Digest>,
    pub origin_snapshot: Option<Digest>,
}

/// Canonical action digest: the committed intent's identity (used by the
/// identical-action breaker and outcome pairing).
pub fn tool_action_digest(intent: &ToolIntent) -> Digest {
    Digest::new(
        serde_json::to_string(&json!({
            "tool": intent.tool,
            "args": canonicalize(intent.args.clone()),
        }))
        .unwrap_or_default()
        .as_bytes(),
    )
}

/// Terminal tool outcome classification (R-02/C-03): dispatched work whose
/// origin is stale is always committed as a fact, classified `interrupted`
/// or `ambiguous`; committed-intent-without-outcome is the sufficient
/// condition for the session to classify `ambiguous` at recovery (B-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeClassification {
    Normal,
    Interrupted(String),
    Ambiguous(String),
}

/// Committed tool outcome: references both the origin snapshot (the intent's
/// world) and the commit snapshot (the FSM/policy environment that accepted
/// it — R-08).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub call_id: ToolCallId,
    pub tool: String,
    pub result: Value,
    pub error: Option<String>,
    pub classification: OutcomeClassification,
    pub origin_snapshot: Option<Digest>,
    pub commit_snapshot: Option<Digest>,
    /// Output retention candidate decisions are applied by the session's
    /// retention gate; the outcome records the admission.
    pub retained: Option<bool>,
}

/// A tool intent parked in the session's bounded approval queue (R-17/H-05):
/// the committed intent plus the approval intent whose digest gates it. On
/// overflow the oldest entry is evicted and resolves `Interrupted`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalParked {
    pub intent: ToolIntent,
    pub approval: ApprovalIntent,
}

// ---------- approval binding (R-16/D-12) ----------

/// Build the approval intent for a tool call: binds tool ModuleId+generation,
/// action type, canonicalized arguments, and (for process tools) cwd/env
/// fingerprint, domain-separated under `approval-v1` (capabilities crate).
pub fn approval_for(
    principal: &Principal,
    action: &str,
    args: &Value,
    cwd_env_fingerprint: Option<String>,
    scope: kanbei_capabilities::GrantScope,
    expiry: Option<u64>,
) -> ApprovalIntent {
    let intent = ApprovalIntent {
        digest: Digest::new(b""), // recomputed below
        principal: principal.clone(),
        module_generation: principal.generation,
        action: action.to_string(),
        args: canonicalize(args.clone()),
        cwd_env_fingerprint,
        scope,
        expiry,
    };
    ApprovalIntent {
        digest: intent.derive_digest(),
        ..intent
    }
}

/// Process-tool cwd/env fingerprint (R-16/D-12): canonical join of cwd +
    /// sorted env pairs.
pub fn cwd_env_fingerprint(cwd: &str, env: &BTreeMap<String, String>) -> String {
    let mut parts: Vec<String> = vec![format!("cwd={cwd}")];
    parts.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
    parts.join("\n")
}

// ---------- native executors ----------

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool {0}: unknown tool")]
    UnknownTool(String),
    #[error("tool {0}: unavailable — {1}")]
    Unavailable(String, String),
    #[error("tool {0}: invalid arguments — {1}")]
    InvalidArgs(String, String),
    #[error("tool {0}: io error — {1}")]
    Io(String, String),
    #[error("tool {0}: process timed out after {1}ms")]
    Timeout(String, u64),
    #[error("tool {0}: output limit exceeded")]
    OutputLimit(String),
}

/// Shared execution limits for native tools.
#[derive(Debug, Clone, Copy)]
pub struct ExecLimits {
    pub max_read_bytes: usize,
    pub max_search_results: usize,
    pub max_process_output: usize,
    pub max_process_timeout_ms: u64,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 1 << 20,
            max_search_results: 512,
            max_process_output: 1 << 20,
            max_process_timeout_ms: 120_000,
        }
    }
}

/// Recursive canonicalization: object keys sorted bytewise.
pub fn canonicalize(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, canonicalize(v));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

/// Native tool executor: pure fs/git/todo operations plus the process
/// launcher. Memory tools dispatch to explicit `Unavailable` until M4.
#[derive(Default)]
pub struct NativeTools {
    pub limits: ExecLimits,
    pub todo: TodoStore,
}

/// Session-hosted todo/task state (host-owned typed state; M2 module-state
/// pattern).
#[derive(Debug, Clone, Default)]
pub struct TodoStore {
    items: BTreeMap<String, String>,
}

impl TodoStore {
    pub fn list(&self) -> Vec<(String, String)> {
        self.items.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn update(&mut self, key: &str, status: &str) {
        self.items.insert(key.to_string(), status.to_string());
    }
}

/// Execute one tool. `fs_root` bounds the fs tools' reachable root (the
/// session cwd); absolute paths outside it are rejected.
pub fn execute_tool(
    tools: &mut NativeTools,
    registry: &ToolRegistry,
    name: &str,
    args: &Value,
    fs_root: &Path,
) -> Result<Value, ToolError> {
    if registry.get(name).is_none() {
        return Err(ToolError::UnknownTool(name.into()));
    }
    let args = canonicalize(args.clone());
    match name {
        "fs.read" => {
            let path = str_arg(&args, "path")?;
            let p = resolve(fs_root, &path)?;
            let meta = std::fs::metadata(&p).map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            if !meta.is_file() {
                return Err(ToolError::InvalidArgs(name.into(), "not a file".into()));
            }
            let mut buf = Vec::new();
            let f = std::fs::File::open(&p).map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            f.take((tools.limits.max_read_bytes + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            if buf.len() > tools.limits.max_read_bytes {
                return Err(ToolError::OutputLimit(name.into()));
            }
            let content = String::from_utf8_lossy(&buf).to_string();
            Ok(json!({"content": content, "bytes": buf.len()}))
        }
        "fs.search" => {
            let root = str_arg(&args, "root")?;
            let query = str_arg(&args, "query")?;
            let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let p = resolve(fs_root, &root)?;
            let mut matches = Vec::new();
            walk(&p, &query, max, &mut matches)
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            Ok(json!({"matches": matches}))
        }
        "fs.write" => {
            let path = str_arg(&args, "path")?;
            let content = str_arg(&args, "content")?;
            let p = resolve(fs_root, &path)?;
            let bytes = content.len();
            atomic_write(&p, content.as_bytes())
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            Ok(json!({"bytes": bytes}))
        }
        "fs.patch" => {
            let path = str_arg(&args, "path")?;
            let p = resolve(fs_root, &path)?;
            let replacements = args
                .get("replacements")
                .and_then(|v| v.as_array())
                .ok_or_else(|| ToolError::InvalidArgs(name.into(), "replacements array".into()))?;
            let mut content = std::fs::read_to_string(&p)
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            let mut applied = 0;
            for r in replacements {
                let old = r.get("old").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidArgs(name.into(), "replacement.old string".into())
                })?;
                let new = r.get("new").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidArgs(name.into(), "replacement.new string".into())
                })?;
                let count = content.matches(old).count();
                if count != 1 {
                    return Err(ToolError::InvalidArgs(
                        name.into(),
                        format!("replacement target matches {count} times (must be exactly 1)"),
                    ));
                }
                content = content.replacen(old, new, 1);
                applied += 1;
            }
            atomic_write(&p, content.as_bytes())
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            Ok(json!({"applied": applied}))
        }
        "git.status" | "git.diff" => {
            let repo = str_arg(&args, "repo")?;
            let p = resolve(fs_root, &repo)?;
            let max = args
                .get("max_output")
                .and_then(|v| v.as_u64())
                .unwrap_or(tools.limits.max_process_output as u64) as usize;
            let sub = if name == "git.status" { "status" } else { "diff" };
            let out = run_git(&p, &[sub], max, tools.limits.max_process_timeout_ms)
                .map_err(|e| ToolError::Io(name.into(), e.to_string()))?;
            Ok(json!({"output": out}))
        }
        "process.exec" => {
            let argv: Vec<String> = args
                .get("argv")
                .and_then(|v| v.as_array())
                .ok_or_else(|| ToolError::InvalidArgs(name.into(), "argv array".into()))?
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if argv.is_empty() {
                return Err(ToolError::InvalidArgs(name.into(), "empty argv".into()));
            }
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| fs_root.to_string_lossy().to_string());
            let cwd_p = resolve(fs_root, &cwd)?;
            let mut env = BTreeMap::new();
            if let Some(e) = args.get("env").and_then(|v| v.as_object()) {
                for (k, v) in e {
                    if let Some(s) = v.as_str() {
                        env.insert(k.clone(), s.to_string());
                    }
                }
            }
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(tools.limits.max_process_timeout_ms)
                .min(tools.limits.max_process_timeout_ms);
            let max_out = args
                .get("max_output")
                .and_then(|v| v.as_u64())
                .unwrap_or(tools.limits.max_process_output as u64)
                .min(tools.limits.max_process_output as u64) as usize;
            let result = run_process(&argv[0], &argv[1..], &cwd_p, &env, timeout_ms, max_out)
                .map_err(|e| match e {
                    ProcessErr::Timeout => ToolError::Timeout(name.into(), timeout_ms),
                    ProcessErr::OutputLimit => ToolError::OutputLimit(name.into()),
                    ProcessErr::Io(s) => ToolError::Io(name.into(), s),
                })?;
            Ok(json!({
                "exit": result.exit,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "timed_out": result.timed_out,
            }))
        }
        "todo.list" => {
            let items: Vec<Value> = tools
                .todo
                .list()
                .into_iter()
                .map(|(k, v)| json!({"key": k, "status": v}))
                .collect();
            Ok(json!({"items": items}))
        }
        "todo.update" => {
            let key = str_arg(&args, "key")?;
            let status = str_arg(&args, "status")?;
            tools.todo.update(&key, &status);
            Ok(json!({"ok": true}))
        }
        "child.spawn" => Err(ToolError::Unavailable(
            "child.spawn".into(),
            "child spawn dispatch is wired by the session scheduler".into(),
        )),
        "memory.query" | "memory.propose" => Err(ToolError::Unavailable(
            name.into(),
            "memory substrate lands in M4".into(),
        )),
        _ => Err(ToolError::UnknownTool(name.into())),
    }
}

// ---------- helpers ----------

fn str_arg(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ToolError::InvalidArgs("args".into(), format!("{key}: string required")))
}

/// Resolve a possibly-relative path against the fs root; absolute paths
/// outside the root are rejected (fs tools never escape the session root).
fn resolve(root: &Path, p: &str) -> Result<PathBuf, ToolError> {
    let cand = Path::new(p);
    let joined = if cand.is_absolute() {
        cand.to_path_buf()
    } else {
        root.join(cand)
    };
    let root_abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let joined_abs = joined.canonicalize().unwrap_or(joined.clone());
    if !joined_abs.starts_with(&root_abs) {
        return Err(ToolError::InvalidArgs(
            "fs".into(),
            format!("path escapes session root: {p}"),
        ));
    }
    Ok(joined_abs)
}

fn walk(dir: &Path, query: &str, max: usize, out: &mut Vec<String>) -> std::io::Result<()> {
    if out.len() >= max {
        return Ok(());
    }
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, query, max, out)?;
            } else if path.to_string_lossy().contains(query) {
                out.push(path.to_string_lossy().to_string());
                if out.len() >= max {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Atomic write: temp + rename in the same directory (no fsync — module
/// state semantics; canonical objects use the store protocol).
fn atomic_write(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

fn run_git(repo: &Path, args: &[&str], max_out: usize, timeout_ms: u64) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    cmd.env_clear();
    cmd.env("GIT_PAGER", "cat");
    cmd.stdin(Stdio::null());
    let out = run_cmd(&mut cmd, timeout_ms, max_out)
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if out.timed_out {
        return Err("git timed out".into());
    }
    Ok(out.stdout)
}

// ---------- process launcher (R-28/D-S2) ----------

#[derive(Debug)]
pub struct ProcessResult {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessErr {
    #[error("process timed out")]
    Timeout,
    #[error("process output limit exceeded")]
    OutputLimit,
    #[error("process io error: {0}")]
    Io(String),
}

/// Launch controls (R-28/D-S2): default-deny environment (env_clear +
/// allowlist), inherited-FD closure (stdio piped, stdin null), timeout with
/// process-tree cancellation, bounded output.
pub fn run_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
    max_out: usize,
) -> Result<ProcessResult, ProcessErr> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Process-group leader so the whole tree can be cancelled.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    run_cmd(&mut cmd, timeout_ms, max_out)
}

/// Shared runner: bounded read of stdout/stderr, timeout with tree kill.
pub fn run_cmd(
    cmd: &mut Command,
    timeout_ms: u64,
    max_out: usize,
) -> Result<ProcessResult, ProcessErr> {
    let mut child = cmd.spawn().map_err(|e| ProcessErr::Io(e.to_string()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let started = Instant::now();
    let mut out_reader = stdout.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            buf
        })
    });
    let mut err_reader = stderr.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            buf
        })
    });

    loop {
        if let Some(status) = child.try_wait().map_err(|e| ProcessErr::Io(e.to_string()))? {
            let exit = status.code().unwrap_or(-1);
            let out = out_reader.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            let err = err_reader.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            if out.len() > max_out || err.len() > max_out {
                return Err(ProcessErr::OutputLimit);
            }
            return Ok(ProcessResult {
                exit,
                stdout: String::from_utf8_lossy(&out).to_string(),
                stderr: String::from_utf8_lossy(&err).to_string(),
                timed_out: false,
            });
        }
        if started.elapsed() > Duration::from_millis(timeout_ms) {
            #[cfg(unix)]
            unsafe {
                let _ = libc_killpg(child.id() as i32);
            }
            let _ = child.kill();
            let _ = child.wait();
            let out = out_reader.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            let err = err_reader.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            return Ok(ProcessResult {
                exit: -1,
                stdout: String::from_utf8_lossy(&out).to_string(),
                stderr: String::from_utf8_lossy(&err).to_string(),
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Unreachable: the loop returns on exit or timeout; keep the signature
    // honest for the compiler.
    #[allow(unreachable_code)]
    Err(ProcessErr::Timeout)
}

#[cfg(unix)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn libc_killpg(pid: i32) -> i32 {
    // Negative pid = process group kill (SIGKILL).
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(-pid, 9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kb-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tools() -> NativeTools {
        NativeTools::default()
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::builtin()
    }

    #[test]
    fn registry_deterministic() {
        let r1 = ToolRegistry::builtin();
        let r2 = ToolRegistry::builtin();
        assert_eq!(r1.digest(), r2.digest());
        let names = r1.names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(r1.get("fs.read").is_some());
        assert!(r1.get("nope").is_none());
    }

    #[test]
    fn fs_read_write_patch_roundtrip() {
        let root = tmpdir("fs");
        let mut t = tools();
        let reg = registry();
        let w = execute_tool(
            &mut t,
            &reg,
            "fs.write",
            &json!({"path": "a.txt", "content": "hello world"}),
            &root,
        )
        .unwrap();
        assert_eq!(w["bytes"], 11);
        let r = execute_tool(&mut t, &reg, "fs.read", &json!({"path": "a.txt"}), &root).unwrap();
        assert_eq!(r["content"], "hello world");
        let p = execute_tool(
            &mut t,
            &reg,
            "fs.patch",
            &json!({"path": "a.txt", "replacements": [{"old": "world", "new": "kanbei"}]}),
            &root,
        )
        .unwrap();
        assert_eq!(p["applied"], 1);
        let r = execute_tool(&mut t, &reg, "fs.read", &json!({"path": "a.txt"}), &root).unwrap();
        assert_eq!(r["content"], "hello kanbei");
    }

    #[test]
    fn fs_tools_never_escape_root() {
        let root = tmpdir("escape");
        let mut t = tools();
        let reg = registry();
        let outside = std::env::temp_dir().join(format!("kb-outside-{}", std::process::id()));
        std::fs::write(&outside, b"x").unwrap();
        let err = execute_tool(
            &mut t,
            &reg,
            "fs.read",
            &json!({"path": outside.to_string_lossy()}),
            &root,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_, _)));
    }

    #[test]
    fn patch_requires_unique_target() {
        let root = tmpdir("patch");
        let mut t = tools();
        let reg = registry();
        execute_tool(
            &mut t,
            &reg,
            "fs.write",
            &json!({"path": "a.txt", "content": "x x x"}),
            &root,
        )
        .unwrap();
        let err = execute_tool(
            &mut t,
            &reg,
            "fs.patch",
            &json!({"path": "a.txt", "replacements": [{"old": "x", "new": "y"}]}),
            &root,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_, _)));
    }

    #[test]
    fn search_finds_matches() {
        let root = tmpdir("search");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"").unwrap();
        std::fs::write(root.join("README.md"), b"").unwrap();
        let mut t = tools();
        let reg = registry();
        let r = execute_tool(&mut t, &reg, "fs.search", &json!({"root": ".", "query": "main"}), &root).unwrap();
        let matches = r["matches"].as_array().unwrap();
        assert!(matches.iter().any(|m| m.as_str().unwrap().ends_with("main.rs")));
    }

    #[test]
    fn todo_state_roundtrip() {
        let mut t = tools();
        let reg = registry();
        let u = execute_tool(&mut t, &reg, "todo.update", &json!({"key": "k1", "status": "done"}), Path::new("/")).unwrap();
        assert_eq!(u["ok"], true);
        let l = execute_tool(&mut t, &reg, "todo.list", &json!({}), Path::new("/")).unwrap();
        assert_eq!(l["items"][0]["status"], "done");
    }

    #[test]
    fn memory_tools_explicitly_unavailable() {
        let mut t = tools();
        let reg = registry();
        let err = execute_tool(&mut t, &reg, "memory.query", &json!({"query": "x"}), Path::new("/")).unwrap_err();
        assert!(matches!(err, ToolError::Unavailable(_, _)));
    }

    #[test]
    fn unknown_tool_rejected() {
        let mut t = tools();
        let reg = registry();
        let err = execute_tool(&mut t, &reg, "nope", &json!({}), Path::new("/")).unwrap_err();
        assert!(matches!(err, ToolError::UnknownTool(_)));
    }

    #[test]
    fn process_launch_controls() {
        let root = tmpdir("proc");
        let mut t = tools();
        let reg = registry();
        let r = execute_tool(
            &mut t,
            &reg,
            "process.exec",
            &json!({"argv": ["/bin/sh", "-c", "echo hi"], "cwd": "."}),
            &root,
        )
        .unwrap();
        assert_eq!(r["exit"], 0);
        assert_eq!(r["stdout"], "hi\n");
        assert_eq!(r["timed_out"], false);
    }

    #[test]
    fn process_timeout_kills_tree() {
        let root = tmpdir("timeout");
        let mut t = tools();
        let reg = registry();
        let r = execute_tool(
            &mut t,
            &reg,
            "process.exec",
            &json!({"argv": ["/bin/sh", "-c", "/run/current-system/sw/bin/sleep 30"], "cwd": ".", "timeout_ms": 200}),
            &root,
        )
        .unwrap();
        assert_eq!(r["timed_out"], true);
    }

    #[test]
    fn process_env_is_default_deny() {
        let root = tmpdir("env");
        let mut t = tools();
        let reg = registry();
        unsafe { std::env::set_var("KANBEI_SECRET_TEST_VAR", "leak") };
        let r = execute_tool(
            &mut t,
            &reg,
            "process.exec",
            &json!({"argv": ["/bin/sh", "-c", "echo ${KANBEI_SECRET_TEST_VAR:-unset}"], "cwd": "."}),
            &root,
        )
        .unwrap();
        assert_eq!(r["stdout"], "unset\n");
    }

    #[test]
    fn canonicalize_sorts_keys_recursively() {
        let v = json!({"b": 1, "a": {"d": 2, "c": 3}});
        let c = canonicalize(v);
        assert_eq!(c, json!({"a": {"c": 3, "d": 2}, "b": 1}));
        assert_eq!(serde_json::to_string(&c).unwrap(), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn approval_digest_binding() {
        let p = Principal { session: Id128::generate(), generation: 1, run: Some(7) };
        let a1 = approval_for(
            &p,
            "fs.write",
            &json!({"path": "/a", "content": "x"}),
            None,
            kanbei_capabilities::GrantScope::Run,
            None,
        );
        let a2 = approval_for(
            &p,
            "fs.write",
            &json!({"path": "/a", "content": "y"}),
            None,
            kanbei_capabilities::GrantScope::Run,
            None,
        );
        assert_ne!(a1.digest, a2.digest, "args must bind into the digest");
        assert!(a1.validate());
        let a3 = approval_for(
            &p,
            "process.exec",
            &json!({"argv": ["rm", "-rf", "/"]}),
            Some(cwd_env_fingerprint("/repo", &BTreeMap::from([("A".into(), "1".into())]))),
            kanbei_capabilities::GrantScope::Session,
            Some(1_700_000_000),
        );
        assert!(a3.validate());
        let a4 = approval_for(
            &p,
            "process.exec",
            &json!({"argv": ["rm", "-rf", "/"]}),
            Some(cwd_env_fingerprint("/repo", &BTreeMap::from([("A".into(), "2".into())]))),
            kanbei_capabilities::GrantScope::Session,
            Some(1_700_000_000),
        );
        assert_ne!(a3.digest, a4.digest, "cwd/env fingerprint must bind");
    }

    #[test]
    fn tool_action_digest_stable() {
        let p = Principal { session: Id128::generate(), generation: 1, run: None };
        let i1 = ToolIntent {
            call_id: tool_call_id(),
            run_id: Id128::generate(),
            principal: p.clone(),
            tool: "fs.read".into(),
            args: json!({"path": "/a", "z": 1, "a": 2}),
            approval: None,
            origin_snapshot: None,
        };
        let i2 = ToolIntent {
            call_id: tool_call_id(),
            run_id: Id128::generate(),
            principal: p,
            tool: "fs.read".into(),
            args: json!({"a": 2, "z": 1, "path": "/a"}),
            approval: None,
            origin_snapshot: None,
        };
        assert_eq!(tool_action_digest(&i1), tool_action_digest(&i2));
    }

    #[test]
    fn record_roundtrip() {
        let intent = ToolIntent {
            call_id: tool_call_id(),
            run_id: Id128::generate(),
            principal: Principal { session: Id128::generate(), generation: 0, run: Some(1) },
            tool: "fs.read".into(),
            args: json!({"path": "/a"}),
            approval: None,
            origin_snapshot: Some(Digest::new(b"s")),
        };
        let back: ToolIntent =
            serde_json::from_str(&serde_json::to_string(&intent).unwrap()).unwrap();
        assert_eq!(back, intent);
        let outcome = ToolOutcome {
            call_id: intent.call_id,
            tool: "fs.read".into(),
            result: json!({"content": "x"}),
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: Some(Digest::new(b"s")),
            commit_snapshot: Some(Digest::new(b"c")),
            retained: Some(true),
        };
        let back: ToolOutcome =
            serde_json::from_str(&serde_json::to_string(&outcome).unwrap()).unwrap();
        assert_eq!(back, outcome);
    }
}
