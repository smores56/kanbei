//! The M7 dogfooding battery driver (docs/dogfooding-instrument.md). Runs the
//! six battery tasks against real fixture repositories through the real
//! session kernel (tool FSM, approval broker, memory substrate, checkpoint/
//! continue_from, SIGKILL recovery) with scripted cognition, and computes
//! every metric from canonical session-log records only.
//!
//! Metric provenance (instrument §5): terminal outcomes come from
//! `run_outcome` events, breakers from `breaker_tripped`, intent/outcome
//! pairing from `tool_intent`/`tool_outcome`/`intent_classified`, spend from
//! the `egress` entry inside `model_outcome`. Nothing is derived from
//! self-assessment or SQLite.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kanbei_capabilities::{Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_provider::{CompletionResponse, FakeEngine, FinishReason, KeySource, ProviderConfig, Usage};
use kanbei_scheduler::{
    BreakerFloors, Budgets, StepCommand, StepContext, StepError, StepResult,
    TerminalOutcome, Trigger, TriggerKind,
};
use kanbei_session::{CheckpointRef, Session, SessionConfig};
use serde_json::json;

use crate::fixture::{NOTES_A_V2, NOTES_B_V2, python_path, tool_env};

// ---------- session plumbing ----------

/// The battery broker: every tool the scripted agent needs, granted to the
/// session principal; `memory.propose` additionally requires approval so
/// proposals transition the project root (the gate_m4 pattern).
pub fn battery_broker(session_id: Id128) -> Broker {
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: [
                "fs.read", "fs.search", "fs.write", "fs.patch", "git.status", "git.diff",
                "process.exec", "todo.update", "memory.query", "memory.propose",
            ]
            .iter()
            .map(|r| Capability::new((*r).into(), vec!["call".into()]))
            .collect(),
            deny: vec![],
            require_approval: vec![Capability::new("memory.propose".into(), vec!["call".into()])],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    for resource in [
        "fs.read", "fs.search", "fs.write", "fs.patch", "git.status", "git.diff", "process.exec",
        "todo.update", "memory.query", "memory.propose",
    ] {
        let mut grant = Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: Capability::new(resource.into(), vec!["call".into()]),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("m7-battery".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

fn fake_config() -> ProviderConfig {
    ProviderConfig {
        provider: "fake".into(),
        model: "dogfood".into(),
        base_url: "http://localhost:0/v1".into(),
        key: KeySource::Env("KANBEI_TEST_KEY".into()),
        temperature: None,
        max_tokens: Some(64),
        timeout: Duration::from_secs(5),
    }
}

fn response(content: &str, input: u64, output: u64) -> CompletionResponse {
    CompletionResponse {
        content: Some(content.into()),
        tool_calls: vec![],
        finish_reason: FinishReason::Stop,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
        },
        discontinuity: None,
        opaque_artifacts: None,
    }
}

/// A session with the battery broker, bounded budgets, the repo as fs_root,
/// and a scripted fake engine.
pub fn battery_session(
    dir: &Path,
    repo: &Path,
    session_id: Id128,
    project: Id128,
    responses: Vec<CompletionResponse>,
) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        provider: Some(fake_config()),
        provider_engine: if responses.is_empty() {
            None
        } else {
            Some(Box::new(FakeEngine::new(fake_config(), responses)))
        },
        broker: battery_broker(session_id),
        // unattended battery: the harness approves on the user's behalf
        approval_resolver: Some(std::sync::Arc::new(|_| true)),
        session_id: Some(session_id),
        project: Some(project),
        memory_root: Some(dir.join("memory")),
        fs_root: repo.to_path_buf(),
        budgets: Budgets {
            deadline_secs: Some(300),
            tokens: Some(500_000),
            tools: Some(200),
            children: Some(0),
        },
        breaker_floors: BreakerFloors::default(),
        ..Default::default()
    })
    .unwrap()
}

/// A scripted cognition provider over a fixed command plan.
struct ScriptedProvider {
    commands: VecDeque<StepCommand>,
}

impl kanbei_scheduler::CognitionProvider for ScriptedProvider {
    fn step(
        &mut self,
        _context: &StepContext,
        _trigger: &Trigger,
        _last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError> {
        self.commands
            .pop_front()
            .ok_or(StepError::Invalid("no more commands".into()))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Drive one accepted wake with the given plan; renders through the real
/// projection pipeline. Returns the terminal outcome.
#[allow(clippy::result_large_err)]
pub fn run_wake(session: &mut Session, plan: Vec<StepCommand>) -> TerminalOutcome {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().expect("wake accepted");
    let trigger = run.trigger.clone();
    let run_id = run.run_id;
    session.run_start(run_id).unwrap();
    let mut provider = ScriptedProvider {
        commands: plan.into(),
    };
    session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap()
}

/// The canonical plan pieces (absolute python path resolved once).
fn python() -> String {
    python_path()
}

// ---------- battery task plans ----------

pub fn task1_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t1"),
            max_tokens: None,
        }),
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "test_mathlib.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "mathlib.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.patch".into(),
            arguments: json!({"path": "mathlib.py", "replacements": [
                {"old": "        return hi\n    return lo\n", "new": "        return hi\n    return x\n"}
            ]}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_mathlib"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'fix clamp in-range bug'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

pub fn task2_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t2"),
            max_tokens: None,
        }),
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "test_mathlib.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "mathlib.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.patch".into(),
            arguments: json!({"path": "mathlib.py", "replacements": [
                {"old": "def fib(n):\n    raise NotImplementedError(\"fib lands in M7 task 2\")\n",
                 "new": "def fib(n):\n    if n < 2:\n        return n\n    a, b = 0, 1\n    for _ in range(2, n + 1):\n        a, b = b, a + b\n    return b\n"}
            ]}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_mathlib"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'add fib implementation'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

pub fn task3_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t3"),
            max_tokens: None,
        }),
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "csvlib.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.patch".into(),
            arguments: json!({"path": "csvlib.py", "replacements": [
                {"old": crate::fixture::CSVLIB, "new": crate::fixture::CSVLIB_REFACTORED}
            ]}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_csvlib"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'refactor parse_csv_line'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

pub fn task4_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t4"),
            max_tokens: None,
        }),
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_integration"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "state.py"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "state.json"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.write".into(),
            arguments: json!({"path": "investigation.md", "content":
                "# Investigation: integration test failure\n\n\
                 ## Root cause\n\
                 `state.save_state` writes non-atomically (mode \"w\" truncates before writing); a crash between truncate and write leaves a torn tail, destroying the previous durable state.\n\n\
                 ## Evidence\n\
                 - `python3 -m unittest test_integration` fails with a JSON decode error on state.json.\n\
                 - state.json after simulate_crash contains the torn prefix `{\"par` (read via fs.read).\n\
                 - state.py save_state opens with mode \"w\" (truncate-then-write).\n\n\
                 ## Fix proposal\n\
                 Write to a temp file and os.replace (atomic rename) before publishing.\n"}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'add investigation report'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

pub fn task5a_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t5a"),
            max_tokens: None,
        }),
        StepCommand::ToolIntent {
            tool: "fs.write".into(),
            arguments: json!({"path": "mathlib.py", "content":
                "def gcd(a, b):\n    while b:\n        a, b = b, a % b\n    return a\n\n# gcd ends here\n"}),
        },
        StepCommand::ToolIntent {
            tool: "fs.write".into(),
            arguments: json!({"path": "test_mathlib.py", "content":
                "import unittest\nfrom mathlib import gcd\n\nclass GcdTests(unittest.TestCase):\n    def test_pairs(self):\n        self.assertEqual(gcd(12, 8), 4)\n        self.assertEqual(gcd(17, 5), 1)\n        self.assertEqual(gcd(0, 7), 7)\n\nif __name__ == \"__main__\":\n    unittest.main()\n# gcd tests end\n"}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_mathlib"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'add gcd'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::MemoryPropose {
            claim: json!({"kind": "decision", "content":
                "gcd implemented in mathlib.py with Euclid's algorithm", "sensitivity": "public"}),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

pub fn task5b_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"t5b"),
            max_tokens: None,
        }),
        StepCommand::MemoryQuery {
            query: "gcd implemented".into(),
        },
        StepCommand::ToolIntent {
            tool: "fs.patch".into(),
            arguments: json!({"path": "mathlib.py", "replacements": [
                {"old": "# gcd ends here\n",
                 "new": "# gcd ends here\n\n\ndef lcm(a, b):\n    return a * b // gcd(a, b)\n"}
            ]}),
        },
        StepCommand::ToolIntent {
            tool: "fs.patch".into(),
            arguments: json!({"path": "test_mathlib.py", "replacements": [
                {"old": "from mathlib import gcd\n",
                 "new": "from mathlib import gcd, lcm\n"},
                {"old": "# gcd tests end\n",
                 "new": "# gcd tests end\n\n\nclass LcmTests(unittest.TestCase):\n    def test_lcm(self):\n        self.assertEqual(lcm(4, 6), 12)\n        self.assertEqual(lcm(21, 6), 42)\n"}
            ]}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_mathlib"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add -A && git commit -q -m 'add lcm'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

/// The interrupted task's plan. The slow unittest step (s4) widens the
/// intent-committed/outcome-uncommitted window the harness kills inside.
pub fn task6_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ToolIntent {
            tool: "fs.write".into(),
            arguments: json!({"path": "notes_a.txt", "content": NOTES_A_V2}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add notes_a.txt && git commit -q -m 'update alpha'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "fs.write".into(),
            arguments: json!({"path": "notes_b.txt", "content": NOTES_B_V2}),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": [python(), "-m", "unittest", "test_slow"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::ToolIntent {
            tool: "process.exec".into(),
            arguments: json!({
                "argv": ["/bin/sh", "-c", "git add notes_b.txt && git commit -q -m 'update beta'"],
                "cwd": ".", "env": tool_env(),
            }),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

// ---------- canonical-record extraction ----------

/// One canonical run outcome, extracted from `run_outcome` events.
#[derive(Debug, Clone)]
pub struct RunOutcomeEntry {
    pub seq: u64,
    pub outcome: String,
    pub reason: Option<String>,
}

/// The canonical facts a battery session produced.
#[derive(Debug, Default, Clone)]
pub struct SessionFacts {
    pub runs: Vec<RunOutcomeEntry>,
    pub wakes: u64,
    pub breaker_trips: u64,
    pub model_calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tool_intents: u64,
    /// call_ids of tool intents with no committed outcome.
    pub unoutcomed_intents: Vec<String>,
    /// call_ids of classified interrupted/ambiguous intents.
    pub classified: Vec<String>,
    /// Claim contents returned by memory.query tool outcomes.
    pub memory_query_hits: Vec<String>,
    /// (seq, tool, serialized canonical args) of every committed tool intent.
    pub intent_tools: Vec<(u64, String, String)>,
    /// Seq of the branch_transition event, when one exists.
    pub branch_transition_seq: Option<u64>,
}

pub fn collect_facts(dir: &Path) -> SessionFacts {
    let mut facts = SessionFacts::default();
    let mut outcomed: Vec<String> = Vec::new();
    // Payloads above inline_max live in the object store as {"$object": ...}
    // markers (M1 §7); resolve them so canonical fields (call_id, tool,
    // args) are readable for every intent/outcome record.
    let store = kanbei_objects::ObjectStore::open(
        &dir.join("objects"),
        std::sync::Arc::new(DurabilityQueue::start("kb-df-facts")),
    )
    .ok();
    for env in crate::collect_envelopes(dir).unwrap() {
        let payload = resolve_payload(store.as_ref(), dir, &env);
        let mut env = env;
        env.payload = payload;
        match env.kind.as_str() {
            "run_outcome" => {
                let outcome = env.payload["outcome"].as_object().and_then(|o| o.keys().next().cloned())
                    .unwrap_or_else(|| "?".into());
                let reason = env.payload["reason"].as_str().map(|s| s.to_string());
                facts.runs.push(RunOutcomeEntry {
                    seq: env.seq,
                    outcome,
                    reason,
                });
            }
            "wake_acceptance" => facts.wakes += 1,
            "breaker_tripped" => facts.breaker_trips += 1,
            "model_outcome" => {
                facts.model_calls += 1;
                if let Some(eg) = env.payload.get("egress") {
                    facts.tokens_in += eg["input_tokens"].as_u64().unwrap_or(0);
                    facts.tokens_out += eg["output_tokens"].as_u64().unwrap_or(0);
                }
            }
            "tool_intent" => {
                facts.tool_intents += 1;
                let tool = env.payload["tool"].as_str().unwrap_or_default().to_string();
                let args = env.payload["args"].to_string();
                facts.intent_tools.push((env.seq, tool, args));
            }
            "branch_transition" => {
                facts.branch_transition_seq = Some(env.seq);
            }
            "tool_outcome" => {
                if let Some(id) = env.payload["call_id"].as_str() {
                    outcomed.push(id.to_string());
                }
                if env.payload["tool"].as_str() == Some("memory.query")
                    && let Some(claims) = env.payload["result"]["claims"].as_array()
                {
                    for c in claims {
                        if let Some(content) = c.get("text").and_then(|v| v.as_str()) {
                            facts.memory_query_hits.push(content.to_string());
                        }
                    }
                }
            }
            "intent_classified" => {
                if let Some(id) = env.payload["call_id"].as_str() {
                    facts.classified.push(id.to_string());
                }
            }
            _ => {}
        }
    }
    for env in crate::collect_envelopes(dir).unwrap() {
        if env.kind == "tool_intent" {
            let payload = resolve_payload(store.as_ref(), dir, &env);
            if let Some(id) = payload["call_id"].as_str()
                && !outcomed.iter().any(|o| o == id)
            {
                facts.unoutcomed_intents.push(id.to_string());
            }
        }
    }
    facts
}

/// Resolve an `{"$object": "blake3:<hex>"}` payload marker to its stored
/// JSON; inline payloads pass through. Missing/unparseable objects resolve
/// to the marker itself (the record is still counted, fields are absent).
fn resolve_payload(store: Option<&kanbei_objects::ObjectStore>, _dir: &Path, env: &crate::Envelope) -> serde_json::Value {
    let Some(marker) = env.payload.get("$object").and_then(|o| o.as_str()) else {
        return env.payload.clone();
    };
    if let Some(store) = store
        && let Ok(digest) = kanbei_core::digest::Digest::from_hex(marker)
        && let Ok(bytes) = store.get(&digest)
        && let Ok(v) = serde_json::from_slice(&bytes)
    {
        return v;
    }
    env.payload.clone()
}

/// Reference rates fixed at ratification (instrument §3).
pub const RATE_INPUT_PER_1M: f64 = 5.0;
pub const RATE_OUTPUT_PER_1M: f64 = 15.0;

pub fn usd(tokens_in: u64, tokens_out: u64) -> f64 {
    tokens_in as f64 / 1e6 * RATE_INPUT_PER_1M + tokens_out as f64 / 1e6 * RATE_OUTPUT_PER_1M
}

// ---------- recovery verification (T2.x) ----------

/// Reopen validity for a dogfood session: the session reopens with the same
/// identity, seqs are contiguous from 1, every referenced object exists, a
/// checkpoint's closure verifies, and appending still works.
pub fn verify_dogfood_recovery(
    dir: &Path,
    repo: &Path,
    session_id: Id128,
    project: Id128,
) -> Result<(), String> {
    // A SIGKILLed session may leave a torn tail; recover truncates it before
    // the append-mode reopen (the M1 contract).
    kanbei_log::recover(&dir.join("log.zst")).map_err(|e| format!("log recover: {e}"))?;
    let mut session = battery_session(dir, repo, session_id, project, Vec::new());
    let evs = crate::collect_envelopes(dir).map_err(|e| e.to_string())?;
    for (i, env) in evs.iter().enumerate() {
        if env.seq != (i as u64 + 1) {
            return Err(format!("seq gap at index {i}: expected {}, got {}", i + 1, env.seq));
        }
    }
    let store = kanbei_objects::ObjectStore::open(
        &dir.join("objects"),
        std::sync::Arc::new(DurabilityQueue::start("kb-df-verify")),
    )
    .map_err(|e| e.to_string())?;
    for digest in crate::referenced_digests(dir).map_err(|e| e.to_string())? {
        if !store.exists(&digest) {
            return Err(format!("dangling reference {digest}"));
        }
    }
    // The post-event manifest closure (M6): the checkpoint payload's digest
    // must verify against the store when a checkpoint exists.
    for env in &evs {
        if env.kind == "checkpoint_created"
            && let Some(digest) = env.payload.get("manifest_digest").and_then(|d| d.as_str())
        {
                let digest = Digest::from_hex(digest).map_err(|e| e.to_string())?;
                let manifest_bytes = store
                    .get(&digest)
                    .map_err(|e| format!("checkpoint manifest {digest}: {e}"))?;
                let manifest: kanbei_snapshot::ExecutionManifest =
                    serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
                let refs = kanbei_snapshot::manifest_closure(&manifest);
                kanbei_snapshot::verify_closure(&store, &refs).map_err(|e| e.to_string())?;
            }
        }
    // Reopen + append still works.
    session
        .commit(
            vec![kanbei_session::NewEvent {
                kind: "recovery_probe".into(),
                payload_schema: 1,
                payload: json!({"ok": true}),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )
        .map_err(|e| e.to_string())?;
    session.close().map_err(|e| e.to_string())?;
    Ok(())
}

/// Classification honesty (T2.2): every committed tool intent without an
/// outcome carries an explicit `intent_classified` fact.
pub fn classification_honest(dir: &Path) -> Result<(), String> {
    let facts = collect_facts(dir);
    let mut unclassified = Vec::new();
    for id in &facts.unoutcomed_intents {
        if !facts.classified.iter().any(|c| c == id) {
            unclassified.push(id.clone());
        }
    }
    if unclassified.is_empty() {
        Ok(())
    } else {
        Err(format!("unclassified intents: {unclassified:?}"))
    }
}

/// git log --oneline for the fixture repo.
pub fn git_log(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("GIT_PAGER", "cat")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect()
}

pub fn git_diff_stat(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("GIT_PAGER", "cat")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------- the interrupted-task child protocol ----------

/// Spawn the task-6 crash child: it runs the full plan in one wake, acking
/// `ready <k>` before dispatching step k (1-based) and `complete` when the
/// run finished.
pub fn spawn_task6_child(dir: &Path, repo: &Path, session_id: Id128, project: Id128) -> Child {
    let exe = crate::crash_child_exe().clone();
    Command::new(exe)
        .env("KANBEI_CRASH_DIR", dir)
        .env("KANBEI_CRASH_MODE", "m7")
        .env("KANBEI_DF_DIR", dir)
        .env("KANBEI_DF_REPO", repo)
        .env("KANBEI_DF_SESSION", session_id.to_string())
        .env("KANBEI_DF_PROJECT", project.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Read the child's ack lines until `want` (e.g. "ready 4") or timeout.
fn wait_ack(child: &mut Child, want: &str, timeout: Duration) -> Result<(), String> {
    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            if tx.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("child did not ack {want} in time"));
        }
        match rx.recv_timeout(remaining) {
            Ok(line) if line == want => return Ok(()),
            Ok(line) if line == "complete" => {
                return Err(format!("child completed before {want} (last ack {line})"))
            }
            Ok(_) => {}
            Err(_) => return Err(format!("child stdout closed before {want}")),
        }
    }
}

/// Kill at the ready-4 window only once the tool intent is committed but not
/// yet outcomed (the deterministic torn window; the slow unittest widens it).
fn wait_torn_intent(dir: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let facts = collect_facts(dir);
        if !facts.unoutcomed_intents.is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(2));
    }
    Err("torn intent never appeared".into())
}

// ---------- the battery itself ----------

#[derive(Debug, Clone)]
pub struct TaskRun {
    pub task: u8,
    pub outcome: TerminalOutcome,
    pub facts: SessionFacts,
    /// Cost of the task at reference rates.
    pub cost_usd: f64,
    /// Wall clock of the task session (includes tool execution + fsync).
    pub elapsed: Duration,
    /// The fixture repo (for success-criteria verification).
    pub repo: PathBuf,
}

#[derive(Debug, Clone)]
pub struct InterruptedRun {
    pub kill: String,
    pub recovery_valid: bool,
    pub resumed: TerminalOutcome,
    pub commits: Vec<String>,
    pub dup_effects: bool,
    pub torn_intents: usize,
}

pub struct BatteryReport {
    pub tasks: Vec<TaskRun>,
    pub interrupted: Vec<InterruptedRun>,
    pub unattended_wakes: u64,
    pub unattended_elapsed: Duration,
    pub unattended_spend: f64,
    pub unattended_hourly_usd: f64,
    pub spend_trip: Option<(u64, u64, bool)>,
    /// (objects on disk, referenced, orphans) for the task-1 session dir.
    pub usage: (u64, u64, u64),
}

fn task_done(
    task: u8,
    outcome: TerminalOutcome,
    dir: &Path,
    repo: &Path,
    started: Instant,
) -> TaskRun {
    let facts = collect_facts(dir);
    let cost_usd = usd(facts.tokens_in, facts.tokens_out);
    TaskRun {
        task,
        outcome,
        facts,
        cost_usd,
        elapsed: started.elapsed(),
        repo: repo.to_path_buf(),
    }
}

/// Task 5: part A session (implement gcd + commit + claim + checkpoint),
/// then a fresh session continuing from the checkpoint for part B (lcm).
pub fn run_task5(root: &Path) -> (TaskRun, TaskRun, CheckpointRef) {
    let repo = crate::fixture::fixture_task5();
    let dir = root.join("t5");
    std::fs::create_dir_all(&dir).unwrap();
    let session_id = Id128::generate();
    let project = Id128::generate();
    let started = Instant::now();
    let mut session = battery_session(
        &dir,
        repo.path(),
        session_id,
        project,
        vec![
            response("part A: plan", 1_200, 180),
            response("part A: done", 400, 60),
        ],
    );
    let outcome = run_wake(&mut session, task5a_plan());
    let checkpoint = session
        .create_checkpoint(Some("part A done".to_string()))
        .expect("checkpoint");
    session.close().unwrap();
    let part_a = task_done(5, outcome, &dir, repo.path(), started);

    let started = Instant::now();
    let mut session = battery_session(
        &dir,
        repo.path(),
        session_id,
        project,
        vec![
            response("part B: plan", 1_000, 150),
            response("part B: done", 300, 50),
        ],
    );
    session.continue_from(&checkpoint).expect("continue_from");
    let outcome = run_wake(&mut session, task5b_plan());
    session.close().unwrap();
    let part_b = task_done(5, outcome, &dir, repo.path(), started);
    (part_a, part_b, checkpoint)
}

/// Task 6: the interrupted-task matrix. Each case spawns a child running the
/// full plan, SIGKILLs it at a chosen window, verifies recovery, resumes the
/// remainder, and checks effects (git log + file contents) for duplicates.
fn run_task6(root: &Path) -> Vec<InterruptedRun> {
    let mut out = Vec::new();
    for (label, kill_at, poll_torn) in [
        ("ready-1", Some(1), false),
        ("ready-2", Some(2), false),
        ("ready-3", Some(3), false),
        ("torn-slow-test", Some(4), true),
        ("ready-5", Some(5), false),
        ("ready-6", Some(6), false),
        ("control", None, false),
    ] {
        let repo = crate::fixture::fixture_task6();
        let dir = root.join(format!("t6-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let mut child = spawn_task6_child(&dir, repo.path(), session_id, project);
        let mut run = InterruptedRun {
            kill: label.into(),
            recovery_valid: false,
            resumed: TerminalOutcome::Waiting,
            commits: Vec::new(),
            dup_effects: false,
            torn_intents: 0,
        };
        if let Some(k) = kill_at {
            let want = format!("ready {k}");
            let ack_ok = wait_ack(&mut child, &want, Duration::from_secs(60));
            if ack_ok.is_ok() {
                if poll_torn {
                    let _ = wait_torn_intent(&dir, Duration::from_secs(30));
                }
                let _ = child.kill();
            }
            let _ = child.wait();
            let ack_ok = ack_ok.map(|_| ());
            if ack_ok.is_err() {
                panic!("task6 {label}: {ack_ok:?}");
            }
        } else {
            // Control: let the child finish on its own.
            let _ = child.wait();
        }
        let facts = collect_facts(&dir);
        run.torn_intents = facts.unoutcomed_intents.len();
        run.recovery_valid = verify_dogfood_recovery(&dir, repo.path(), session_id, project).is_ok()
            && classification_honest(&dir).is_ok();

        // Resume: reopen + run the remainder of the plan.
        let mut session = battery_session(&dir, repo.path(), session_id, project, Vec::new());
        let plan = task6_plan();
        let remainder: Vec<StepCommand> = match kill_at {
            None => vec![],
            Some(k) => plan[(k - 1).min(plan.len() - 1)..].to_vec(),
        };
        if !remainder.is_empty() {
            run.resumed = run_wake(&mut session, remainder);
        } else {
            // Control: the child already completed; just reopen.
            run.resumed = TerminalOutcome::CompletedGoal;
        }
        session.close().unwrap();

        let commits = git_log(repo.path());
        let messages: Vec<String> = commits
            .iter()
            .map(|l| l.split(' ').skip(1).collect::<Vec<_>>().join(" "))
            .collect();
        run.commits = commits;
        run.dup_effects = !(messages.iter().filter(|m| m.as_str() == "update alpha").count() == 1
            && messages.iter().filter(|m| m.as_str() == "update beta").count() == 1);
        let notes_a = std::fs::read_to_string(repo.path().join("notes_a.txt")).unwrap();
        let notes_b = std::fs::read_to_string(repo.path().join("notes_b.txt")).unwrap();
        if notes_a != NOTES_A_V2 || notes_b != NOTES_B_V2 {
            run.dup_effects = true;
        }
        out.push(run);
    }
    out
}

/// The unattended-hour scaled measurement: perpetual cheap cognition at a
/// paced cadence for `duration`, spend extrapolated to an hour (T3.3).
pub fn run_unattended(root: &Path, duration: Duration) -> (u64, Duration, f64, f64) {
    let dir = root.join("unattended");
    std::fs::create_dir_all(&dir).unwrap();
    let session_id = Id128::generate();
    let project = Id128::generate();
    let responses = (0..600)
        .map(|_| response("keep-alive", 100, 25))
        .collect();
    let mut session = battery_session(&dir, &dir, session_id, project, responses);
    let started = Instant::now();
    let mut wakes = 0u64;
    let mut last = Instant::now();
    while started.elapsed() < duration {
        let sleep = Duration::from_secs(3).saturating_sub(last.elapsed());
        thread::sleep(sleep);
        last = Instant::now();
        let plan = vec![
            StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
                rendered_hash: Digest::new(b"unattended"),
                max_tokens: None,
            }),
            StepCommand::Finish(TerminalOutcome::Progress),
        ];
        run_wake(&mut session, plan);
        wakes += 1;
    }
    session.close().unwrap();
    let facts = collect_facts(&dir);
    let spend = usd(facts.tokens_in, facts.tokens_out);
    let elapsed = started.elapsed();
    let hourly = spend * 3600.0 / elapsed.as_secs_f64();
    (wakes, elapsed, spend, hourly)
}

/// The spend-breaker scenario (T3.4): a low token floor trips the spend
/// breaker on the run's own egress; the control with a high floor does not.
pub fn run_spend_scenario(root: &Path) -> (Option<(u64, u64, bool)>, SessionFacts) {
    let dir = root.join("spend");
    std::fs::create_dir_all(&dir).unwrap();
    let session_id = Id128::generate();
    let project = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        provider: Some(fake_config()),
        provider_engine: Some(Box::new(FakeEngine::new(
            fake_config(),
            vec![response("spend", 100, 25)],
        ))),
        broker: battery_broker(session_id),
        // unattended battery: the harness approves on the user's behalf
        approval_resolver: Some(std::sync::Arc::new(|_| true)),
        session_id: Some(session_id),
        project: Some(project),
        memory_root: Some(dir.join("memory")),
        fs_root: dir.clone(),
        budgets: Budgets {
            deadline_secs: Some(60),
            tokens: Some(500_000),
            tools: Some(50),
            children: Some(0),
        },
        breaker_floors: BreakerFloors {
            spend_window_secs: 60,
            spend_tokens: 50,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let plan = vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"spend"),
            max_tokens: None,
        }),
        StepCommand::Finish(TerminalOutcome::Progress),
    ];
    run_wake(&mut session, plan);
    // The trip pauses cognition: a further wake must be denied.
    let denied = {
        session.observe_trigger(Trigger {
            kind: TriggerKind::NewCausalEvent,
            referent: None,
        });
        session.accept_wake().unwrap().is_none()
    };
    session.close().unwrap();
    let facts = collect_facts(&dir);
    let trip = if facts.breaker_trips > 0 {
        let evs = crate::collect_envelopes(&dir).unwrap();
        let trip = evs
            .iter()
            .find(|e| e.kind == "breaker_tripped")
            .expect("breaker_tripped event");
        Some((
            trip.payload["value"].as_u64().unwrap_or(0),
            trip.payload["threshold"].as_u64().unwrap_or(0),
            denied,
        ))
    } else {
        None
    };
    (trip, facts)
}

// ---------- thresholds ----------

/// The manual GC-growth usage check (architecture.md line 610: a manual
/// usage check before the dogfooding gate compensates deferred GC): object
/// store size vs the referenced closure — orphans are allowed (never
/// dangling), but unbounded orphan growth would flag compaction needs.
/// Returns (objects_on_disk, referenced, orphans).
pub fn usage_check(dir: &Path) -> (u64, u64, u64) {
    let store = kanbei_objects::ObjectStore::open(
        &dir.join("objects"),
        std::sync::Arc::new(DurabilityQueue::start("kb-df-usage")),
    )
    .unwrap();
    let on_disk = store.scan().unwrap().len() as u64;
    let referenced = crate::referenced_digests(dir).unwrap().len() as u64;
    let (orphans, _) = store
        .prune_scan(&crate::referenced_digests(dir).unwrap())
        .unwrap();
    (on_disk, referenced, orphans)
}

/// The battery, complete: tasks 1-5 (+ task 5's part B), the interrupted
/// matrix, the unattended scaled hour, and the spend scenario. Threshold
/// evaluation (instrument sections 1-3) is separate so the gate test can
/// assert on the verdict.
pub struct ThresholdVerdict {
    pub t1_1: bool,
    pub t1_2: bool,
    pub t1_3: bool,
    pub t1_4: bool,
    pub t1_5: bool,
    pub t2_1: bool,
    pub t2_2: bool,
    pub t2_3: bool,
    pub t3_1: bool,
    pub t3_2: bool,
    pub t3_3: bool,
    pub t3_4: bool,
}

impl ThresholdVerdict {
    pub fn all(&self) -> bool {
        self.t1_1
            && self.t1_2
            && self.t1_3
            && self.t1_4
            && self.t1_5
            && self.t2_1
            && self.t2_2
            && self.t2_3
            && self.t3_1
            && self.t3_2
            && self.t3_3
            && self.t3_4
    }
}

pub fn evaluate_thresholds(report: &BatteryReport) -> ThresholdVerdict {
    let tasks: Vec<&TaskRun> = report.tasks.iter().collect();
    let runs = tasks.len() as f64;
    let completed = tasks
        .iter()
        .filter(|t| t.outcome == TerminalOutcome::CompletedGoal)
        .count() as f64;
    let failed = tasks
        .iter()
        .filter(|t| matches!(t.outcome, TerminalOutcome::Failed(_)))
        .count() as f64;
    let wakes: u64 = tasks.iter().map(|t| t.facts.wakes).sum();
    let trips: u64 = tasks.iter().map(|t| t.facts.breaker_trips).sum();
    let progress = tasks
        .iter()
        .filter(|t| {
            matches!(t.outcome, TerminalOutcome::Progress | TerminalOutcome::CompletedGoal)
        })
        .count() as f64;
    let stalled = tasks
        .iter()
        .filter(|t| t.outcome == TerminalOutcome::Waiting && t.facts.tool_intents == 0)
        .count() as f64;
    let t1_1 = completed / runs >= 0.8;
    let t1_2 = failed / runs <= 0.05;
    let t1_3 = trips <= wakes / 1000;
    let t1_4 = stalled / runs <= 0.02;
    let t1_5 = progress / runs >= 0.9;

    let interrupted: Vec<&InterruptedRun> = report
        .interrupted
        .iter()
        .filter(|r| r.kill != "control")
        .collect();
    let t2_1 = !interrupted.is_empty()
        && interrupted.iter().all(|r| r.recovery_valid);
    let t2_2 = interrupted.iter().all(|r| r.torn_intents == 0 || r.recovery_valid);
    let resumed_ok = interrupted
        .iter()
        .filter(|r| {
            r.resumed == TerminalOutcome::CompletedGoal && !r.dup_effects
        })
        .count();
    let t2_3 = !interrupted.is_empty()
        && resumed_ok as f64 / interrupted.len() as f64 >= 0.9;

    let t3_1 = report
        .tasks
        .iter()
        .all(|t| t.facts.tokens_in <= 250_000 && t.facts.tokens_out <= 25_000);
    let total: f64 = report.tasks.iter().map(|t| t.cost_usd).sum();
    let t3_2 = total <= 6.00;
    let t3_3 = report.unattended_hourly_usd <= 2.00;
    let t3_4 = match &report.spend_trip {
        Some((value, threshold, denied)) => {
            *value >= *threshold && *denied
        }
        None => false,
    };
    ThresholdVerdict {
        t1_1,
        t1_2,
        t1_3,
        t1_4,
        t1_5,
        t2_1,
        t2_2,
        t2_3,
        t3_1,
        t3_2,
        t3_3,
        t3_4,
    }
}

/// The full battery. Caller picks the root temp dir.
pub fn run_battery(root: &Path, unattended: Duration) -> BatteryReport {
    let mut tasks = Vec::new();
    // Task 1: bug fix from failing test.
    {
        let repo = crate::fixture::fixture_task1();
        let dir = root.join("t1");
        std::fs::create_dir_all(&dir).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let started = Instant::now();
        let mut session = battery_session(
            &dir,
            repo.path(),
            session_id,
            project,
            vec![
                response("task1: plan", 2_000, 300),
                response("task1: fixed", 500, 80),
            ],
        );
        let outcome = run_wake(&mut session, task1_plan());
        session.close().unwrap();
        tasks.push(task_done(1, outcome, &dir, repo.path(), started));
    }
    // Task 2: feature with tests.
    {
        let repo = crate::fixture::fixture_task2();
        let dir = root.join("t2");
        std::fs::create_dir_all(&dir).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let started = Instant::now();
        let mut session = battery_session(
            &dir,
            repo.path(),
            session_id,
            project,
            vec![
                response("task2: plan", 1_800, 260),
                response("task2: done", 450, 70),
            ],
        );
        let outcome = run_wake(&mut session, task2_plan());
        session.close().unwrap();
        tasks.push(task_done(2, outcome, &dir, repo.path(), started));
    }
    // Task 3: behavior-preserving refactor.
    {
        let repo = crate::fixture::fixture_task3();
        let dir = root.join("t3");
        std::fs::create_dir_all(&dir).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let started = Instant::now();
        let mut session = battery_session(
            &dir,
            repo.path(),
            session_id,
            project,
            vec![
                response("task3: plan", 1_500, 220),
                response("task3: done", 380, 60),
            ],
        );
        let outcome = run_wake(&mut session, task3_plan());
        session.close().unwrap();
        tasks.push(task_done(3, outcome, &dir, repo.path(), started));
    }
    // Task 4: investigation report (no code change required).
    {
        let repo = crate::fixture::fixture_task4();
        let dir = root.join("t4");
        std::fs::create_dir_all(&dir).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let started = Instant::now();
        let mut session = battery_session(
            &dir,
            repo.path(),
            session_id,
            project,
            vec![
                response("task4: plan", 2_200, 340),
                response("task4: report written", 600, 90),
            ],
        );
        let outcome = run_wake(&mut session, task4_plan());
        session.close().unwrap();
        tasks.push(task_done(4, outcome, &dir, repo.path(), started));
    }
    // Task 5: cross-session continuity (part A + part B via continue_from).
    {
        let (part_a, part_b, _checkpoint) = run_task5(root);
        tasks.push(part_a);
        tasks.push(part_b);
    }
    // Task 6: interrupted task (crash matrix).
    let interrupted = run_task6(root);

    let (unattended_wakes, unattended_elapsed, unattended_spend, unattended_hourly_usd) =
        run_unattended(root, unattended);
    let (spend_trip, _spend_facts) = run_spend_scenario(root);

    let usage = usage_check(&root.join("t1"));
    BatteryReport {
        tasks,
        interrupted,
        unattended_wakes,
        unattended_elapsed,
        unattended_spend,
        unattended_hourly_usd,
        spend_trip,
        usage,
    }
}

// ---------- report text ----------

pub fn format_report(report: &BatteryReport) -> String {
    let mut s = String::new();
    s.push_str("== M7 dogfooding battery report ==\n");
    for t in &report.tasks {
        s.push_str(&format!(
            "task {}: {:?} ({} runs, {} wakes, {} tools, {} in / {} out tokens, ${:.4}, {:.1}s)\n",
            t.task,
            t.outcome,
            t.facts.runs.len(),
            t.facts.wakes,
            t.facts.tool_intents,
            t.facts.tokens_in,
            t.facts.tokens_out,
            t.cost_usd,
            t.elapsed.as_secs_f64(),
        ));
    }
    s.push_str("interrupted runs:\n");
    for r in &report.interrupted {
        s.push_str(&format!(
            "  {}: recovery={} resumed={:?} dup_effects={} torn_intents={} commits={}\n",
            r.kill,
            r.recovery_valid,
            r.resumed,
            r.dup_effects,
            r.torn_intents,
            r.commits.len(),
        ));
    }
    s.push_str(&format!(
        "unattended: {} wakes in {:.0}s, ${:.4} spend, ${:.2}/hr (scaled)\n",
        report.unattended_wakes,
        report.unattended_elapsed.as_secs_f64(),
        report.unattended_spend,
        report.unattended_hourly_usd,
    ));
    let total: f64 = report.tasks.iter().map(|t| t.cost_usd).sum();
    s.push_str(&format!("battery spend: ${:.4}\n", total));
    s.push_str(&format!(
        "usage check (t1 dir): {} objects on disk, {} referenced, {} orphans\n",
        report.usage.0, report.usage.1, report.usage.2
    ));
    match &report.spend_trip {
        Some((v, th, denied)) => {
            s.push_str(&format!(
                "spend breaker: value={v} threshold={th} paused={denied}\n"
            ));
        }
        None => s.push_str("spend breaker: no trip recorded\n"),
    }
    let v = evaluate_thresholds(report);
    for (name, ok) in [
        ("T1.1 completed>=80%", v.t1_1),
        ("T1.2 failed<=5%", v.t1_2),
        ("T1.3 breakers<=1/1000", v.t1_3),
        ("T1.4 stall<=2%", v.t1_4),
        ("T1.5 progress>=90%", v.t1_5),
        ("T2.1 recovery=100%", v.t2_1),
        ("T2.2 classification=100%", v.t2_2),
        ("T2.3 resume>=90%", v.t2_3),
        ("T3.1 per-task tokens", v.t3_1),
        ("T3.2 battery<=$6.00", v.t3_2),
        ("T3.3 hour<=$2.00", v.t3_3),
        ("T3.4 spend breaker", v.t3_4),
    ] {
        s.push_str(&format!("  {}: {}\n", if ok { "PASS" } else { "FAIL" }, name));
    }
    s
}


