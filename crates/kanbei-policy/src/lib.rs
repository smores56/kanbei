//! M2 retention gate: classification/redaction/retention of candidate bytes
//! before they reach storage/telemetry (docs/architecture.md lines 377-381,
//! 604). This crate owns the hard invariants: policy runs before any
//! harness-controlled sink receives bytes, the candidate size limit cannot be
//! bypassed (phase 1 of [`RetentionGate::admit`] runs before any plugin
//! code), and dropping replay-relevant content yields an explicit
//! non-resumable boundary ([`Admission::NonResumableBoundary`]), never a
//! silent drop (R-04/A-04).
//!
//! The replaceable-policy seam ([`PolicyPlugin`] + [`RetentionGate`]) is
//! defined now; the no-effect policy runtime ([`wasm::WasmPolicyPlugin`],
//! R-28/D-S3) hosts this same trait in the Wasm path with an empty capability
//! import set ([`wasm::DenyAllHost`]). The MVP default policy is
//! [`builtins::StoreAllPolicy`]; the kernel contains no mandatory
//! secret-classification algorithm.

use std::sync::Arc;

use thiserror::Error;

/// Default incoming-candidate size ceiling for [`RetentionGate`] (16 MiB).
pub const DEFAULT_MAX_CANDIDATE_BYTES: usize = 16 * 1024 * 1024;

/// What a policy plugin decides for one candidate. Decided *before* any
/// harness-controlled log/object/SQLite/telemetry sink receives bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionDecision {
    /// Keep the candidate bytes as-is.
    Store,
    /// Keep a transformed copy of the candidate bytes (redaction, truncation,
    /// ...). The gate stores exactly these bytes.
    Transform { bytes: Vec<u8> },
    /// Discard the candidate. On a replay-relevant candidate the gate turns
    /// this into an explicit [`Admission::NonResumableBoundary`] (R-04);
    /// otherwise it becomes [`Admission::Dropped`].
    Drop { reason: String },
    /// Reject the whole execution. Passes through as
    /// [`Admission::Rejected`].
    RejectExecution { reason: String },
}

/// Role of a candidate byte stream. Roles that can enter model context are
/// replay-relevant by default (R-04); `Internal` never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateRole {
    /// Content that enters or reflects model context (prompts, reasoning,
    /// completions).
    ModelContext,
    /// Output of a tool call.
    ToolOutput,
    /// User-supplied input.
    UserInput,
    /// Kernel bookkeeping that never enters model context.
    Internal,
}

/// One candidate byte stream submitted for retention classification.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub role: CandidateRole,
    pub content: Vec<u8>,
    /// Set by the kernel from the conservative default
    /// ([`RetentionGate::replay_relevant`]); a kernel-validated tool manifest
    /// may declare false explicitly. The gate never writes this bit.
    pub replay_relevant: bool,
    pub sensitivity: Option<String>,
    pub media: Option<String>,
}

/// Hard failures of the retention gate — never a silent drop.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// Phase-1 size gate: the candidate exceeds the configured ceiling. The
    /// limit cannot be bypassed: the check runs before any plugin code.
    #[error("candidate {bytes} bytes exceeds limit {limit} bytes")]
    Oversized { bytes: usize, limit: usize },
    /// A plugin failed (or returned an error). `admit` prefixes the plugin
    /// name.
    #[error("policy plugin error: {0}")]
    Plugin(String),
    /// Execution rejection surfaced as an error rather than a decision.
    #[error("execution rejected: {0}")]
    Rejected(String),
    /// Drop-on-replay-relevant without an explicit boundary. The gate
    /// converts this case into [`Admission::NonResumableBoundary`] instead of
    /// returning this error (see [`RetentionGate::admit`]); the variant stays
    /// for plugins and future runtimes that signal the boundary explicitly.
    #[error("non-resumable boundary: {0}")]
    NonResumable(String),
    /// A pattern that cannot be compiled/used by the policy.
    #[error("invalid pattern: {0}")]
    InvalidPattern(String),
}

/// Outcome of [`RetentionGate::admit`]. A replay-relevant candidate is never
/// silently dropped: `Drop` yields [`Admission::NonResumableBoundary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Candidate kept — either as-is (`Store`) or transformed (`Transform`).
    Stored { bytes: Vec<u8> },
    /// Candidate dropped; safe to replay/re-run (not replay-relevant).
    Dropped { reason: String },
    /// Drop of replay-relevant content: an explicit non-resumable boundary.
    /// The session records this as a canonical fact via
    /// [`RetentionGate::boundary_fact`].
    NonResumableBoundary { reason: String },
    /// Plugin returned `RejectExecution`; the run must not proceed.
    Rejected { reason: String },
}

/// Kind of a non-resumable boundary fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryKind {
    /// Replay-relevant content was dropped; the session cannot be resumed or
    /// replayed past this point (R-04).
    NonResumable,
    /// Execution was rejected wholesale; nothing was stored or dropped.
    Rejected,
}

/// Canonical fact recorded by the session for non-resumable boundaries
/// (R-04: dropped model-influential content ⇒ explicit non-resumable
/// boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryFact {
    pub kind: BoundaryKind,
    pub reason: String,
    /// Whether replay-relevant content is implicated: true for `NonResumable`
    /// (only replay-relevant candidates produce one); false for `Rejected` —
    /// rejection loses no content, so no replay relevance is implicated.
    pub replay_relevant: bool,
}

/// The replaceable-policy seam. The kernel enforces no-effect by
/// construction: the trait exposes no capabilities (no network, native
/// process, model, filesystem, memory-write, tool, or install access), only a
/// pure `content -> decision` function over a bounded candidate. The
/// no-effect runtime ([`wasm::WasmPolicyPlugin`]) hosts this same trait in the
/// Wasm path with an empty capability import set;
/// [`is_no_effect`](PolicyPlugin::is_no_effect) is the declarative flag that
/// runtime checks.
pub trait PolicyPlugin: Send + Sync {
    /// Classify one candidate. Must be pure and bounded; the gate runs it
    /// only after the phase-1 size check.
    fn decide(&self, candidate: &Candidate) -> Result<RetentionDecision, PolicyError>;

    /// Stable plugin identifier, included in plugin-error messages and facts.
    fn name(&self) -> &'static str;

    /// Declares the plugin side-effect-free. Defaults to true; native
    /// built-ins are no-effect by construction, the Wasm runtime
    /// ([`wasm::WasmPolicyPlugin`]) enforces it via its empty capability
    /// import set ([`wasm::DenyAllHost`]).
    fn is_no_effect(&self) -> bool {
        true
    }
}

/// Two-phase streaming admission gate (docs/architecture.md line 604).
///
/// Phase 1: the candidate size ceiling — over-limit candidates fail with
/// [`PolicyError::Oversized`] before any plugin code runs, so the limit
/// cannot be bypassed. Phase 2: the plugin's decision, reconciled against the
/// R-04 replay contract.
pub struct RetentionGate {
    plugin: Arc<dyn PolicyPlugin>,
    replay_default: bool,
    max_candidate_bytes: usize,
}

impl RetentionGate {
    /// Gate with the given plugin, the conservative replay default (true),
    /// and the 16 MiB candidate ceiling.
    pub fn new(plugin: Arc<dyn PolicyPlugin>) -> Self {
        Self {
            plugin,
            replay_default: true,
            max_candidate_bytes: DEFAULT_MAX_CANDIDATE_BYTES,
        }
    }

    /// Override the phase-1 candidate size ceiling.
    pub fn with_max_candidate_bytes(mut self, limit: usize) -> Self {
        self.max_candidate_bytes = limit;
        self
    }

    /// Override the replay-relevance default used when a candidate's role is
    /// replay-capable and no declaration is given. Conservative default: true.
    pub fn with_replay_default(mut self, default: bool) -> Self {
        self.replay_default = default;
        self
    }

    /// Two-phase admission. The size check (phase 1) runs before any plugin
    /// code and cannot be bypassed; `decide` (phase 2) then decides. The R-04
    /// replay contract is applied to the decision:
    ///
    /// - `Store` → [`Admission::Stored`] with the original bytes;
    /// - `Transform` → [`Admission::Stored`] with the transformed bytes;
    /// - `Drop` on a replay-relevant candidate → [`Admission::NonResumableBoundary`]
    ///   (an explicit boundary, never a silent drop);
    /// - `Drop` on a non-replay-relevant candidate → [`Admission::Dropped`];
    /// - `RejectExecution` → [`Admission::Rejected`] (passes through).
    ///
    /// Hard failures return [`PolicyError`]: oversized candidates, plugin
    /// errors (with the plugin name), invalid patterns.
    pub fn admit(&self, candidate: Candidate) -> Result<Admission, PolicyError> {
        if candidate.content.len() > self.max_candidate_bytes {
            return Err(PolicyError::Oversized {
                bytes: candidate.content.len(),
                limit: self.max_candidate_bytes,
            });
        }

        let decision = match self.plugin.decide(&candidate) {
            Ok(decision) => decision,
            Err(PolicyError::Plugin(msg)) => {
                return Err(PolicyError::Plugin(format!(
                    "plugin '{}': {msg}",
                    self.plugin.name()
                )))
            }
            // An explicit non-resumable signal is converted into the boundary
            // admission, per the PolicyError::NonResumable contract.
            Err(PolicyError::NonResumable(reason)) => {
                return Ok(Admission::NonResumableBoundary { reason })
            }
            Err(other) => return Err(other),
        };

        // The kernel owns the replay-relevance default: the candidate's bit is
        // the manifest declaration, resolved against the role's conservative
        // default. Internal candidates are never replay-relevant even if a
        // caller mis-sets the bit.
        let relevant = self.replay_relevant(candidate.role, Some(candidate.replay_relevant));

        match decision {
            RetentionDecision::Store => Ok(Admission::Stored {
                bytes: candidate.content,
            }),
            RetentionDecision::Transform { bytes } => Ok(Admission::Stored { bytes }),
            RetentionDecision::Drop { reason } => {
                if relevant {
                    Ok(Admission::NonResumableBoundary { reason })
                } else {
                    Ok(Admission::Dropped { reason })
                }
            }
            RetentionDecision::RejectExecution { reason } => Ok(Admission::Rejected { reason }),
        }
    }

    /// The conservative replay-relevance default (R-04/A-04): roles that can
    /// enter model context are replay-relevant unless the kernel-validated
    /// tool manifest explicitly declares otherwise (`declared`); `Internal`
    /// is never replay-relevant.
    pub fn replay_relevant(&self, role: CandidateRole, declared: Option<bool>) -> bool {
        match role {
            CandidateRole::Internal => false,
            _ => declared.unwrap_or(self.replay_default),
        }
    }

    /// The canonical fact for a non-resumable boundary, for the session to
    /// record. `None` for [`Admission::Stored`] and [`Admission::Dropped`] —
    /// no boundary occurred.
    pub fn boundary_fact(&self, admission: &Admission) -> Option<BoundaryFact> {
        match admission {
            Admission::NonResumableBoundary { reason } => Some(BoundaryFact {
                kind: BoundaryKind::NonResumable,
                reason: reason.clone(),
                // By construction admit only produces this for
                // replay-relevant candidates.
                replay_relevant: true,
            }),
            Admission::Rejected { reason } => Some(BoundaryFact {
                kind: BoundaryKind::Rejected,
                reason: reason.clone(),
                // Rejection loses no content, so no replay relevance is
                // implicated.
                replay_relevant: false,
            }),
            Admission::Stored { .. } | Admission::Dropped { .. } => None,
        }
    }
}

/// Built-in default policies. Defaults are ordinary replaceable modules; the
/// kernel contains no mandatory secret-classification algorithm.
pub mod builtins {
    use std::borrow::Cow;

    use regex::bytes::Regex;

    use super::{Candidate, PolicyError, PolicyPlugin, RetentionDecision};

    /// The MVP default: store every candidate unchanged. name() ==
    /// "store-all".
    #[derive(Debug, Clone, Copy, Default)]
    pub struct StoreAllPolicy;

    impl PolicyPlugin for StoreAllPolicy {
        fn decide(&self, _candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::Store)
        }

        fn name(&self) -> &'static str {
            "store-all"
        }
    }

    /// Simple pattern redaction (docs/architecture.md line 604): replaces
    /// every match of each compiled regex pattern with a fixed replacement.
    /// Uses `regex::bytes::Regex` so arbitrary candidate bytes are handled
    /// (the regex crate is pure Rust, Wasm-compatible, and panic-free on
    /// input — a fit for the no-effect seam). Non-matching candidates are
    /// stored unchanged.
    #[derive(Debug)]
    pub struct PatternRedactionPolicy {
        patterns: Vec<Regex>,
        replacement: String,
    }

    impl PatternRedactionPolicy {
        /// Compiles every pattern up front; an invalid regex yields
        /// [`PolicyError::InvalidPattern`] with the offending pattern in the
        /// message.
        pub fn new(patterns: Vec<String>, replacement: String) -> Result<Self, PolicyError> {
            let patterns = patterns
                .iter()
                .map(|p| {
                    Regex::new(p)
                        .map_err(|e| PolicyError::InvalidPattern(format!("'{p}': {e}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                patterns,
                replacement,
            })
        }
    }

    impl PolicyPlugin for PatternRedactionPolicy {
        fn decide(&self, candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            let mut current: Cow<[u8]> = Cow::Borrowed(&candidate.content);
            let mut matched = false;
            for re in &self.patterns {
                if re.is_match(&current) {
                    matched = true;
                    // is_match guarantees replace_all returned an owned Cow;
                    // into_owned() then moves it out without a copy.
                    let replaced: Vec<u8> =
                        re.replace_all(&current, self.replacement.as_bytes()).into_owned();
                    current = Cow::Owned(replaced);
                }
            }
            if matched {
                Ok(RetentionDecision::Transform {
                    bytes: current.into_owned(),
                })
            } else {
                Ok(RetentionDecision::Store)
            }
        }

        fn name(&self) -> &'static str {
            "pattern-redaction"
        }
    }
}

/// Replaceable policies hosted in the kernel-enforced no-effect Wasm runtime
/// (R-28/D-S3, docs/architecture.md line 378): Luau policy source runs inside
/// a kanbei-vm instance whose only host is [`DenyAllHost`] — the empty
/// capability import set — so a policy cannot reach the network, native
/// processes, the model, the filesystem, memory writes, tools, or
/// installation. A policy that attempts a host call traps and the call fails
/// explicitly (never a silent pass); all other failures (trap, fuel, epoch,
/// timeout, bad decision JSON) are explicit [`PolicyError`]s — fail-closed,
/// nothing unclassified commits (R-04/D-07).
pub mod wasm {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use base64::Engine as _;
    use kanbei_vm::{CompiledModule, GuestError, Host, Instance, Vm, VmConfig};
    use serde::{Deserialize, Serialize};

    use super::{Candidate, CandidateRole, PolicyError, PolicyPlugin, RetentionDecision};

    /// Candidate content ceiling for one `kb_hot` call. Base64 of 700 KiB
    /// (~933 KiB) plus the JSON envelope fits the guest's 1 MiB scratch
    /// (`kanbei-guest::SCRATCH_SIZE`) with room for the result buffer. The
    /// gate's phase-1 ceiling ([`super::RetentionGate`]) still applies first;
    /// this is the wasm path's tighter, documented bound. Over-limit
    /// candidates fail explicitly, never silently.
    pub const MAX_WASM_CONTENT_BYTES: usize = 700 * 1024;

    /// The empty capability import set: every host call is denied. The no-effect
    /// contract is enforced by construction — this is the only host a policy
    /// instance ever sees, so `kb_host_call`/`kb_host_double` always trap.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct DenyAllHost;

    impl Host for DenyAllHost {
        fn call(&self, _generation_token: u64, op: u32, _payload: &str) -> Result<String, String> {
            Err(format!(
                "no-effect policy runtime: host op {op} denied (empty capability set)"
            ))
        }
    }

    /// Policy instances are not session generations; [`DenyAllHost`] ignores
    /// the token.
    const GENERATION_TOKEN: u64 = 0;

    /// VmConfig for policy instances. The stock defaults are unusable here:
    /// luaur accounts fuel per wasm instruction, so the default 1M fuel
    /// budget is exhausted by the `kb_init` of any non-trivial policy source,
    /// and the default 1-tick epoch deadline is an *absolute* deadline — the
    /// watchdog counter grows forever, so any finite value trips every call
    /// once passed (kanbei-vm's own tests set `u64::MAX` for the same
    /// reason). Fuel is therefore the deterministic interrupt: `2^35`
    /// instructions is ~4x the measured ~8.4G cost of a max-size redaction
    /// call, and an infinite loop trips it in ~3 s of guest work. The 5 s
    /// wall-clock timeout, the 64 MiB memory ceiling, the 1 MiB guest
    /// scratch, and the gate's phase-1 candidate ceiling back it up. A
    /// runaway policy fails closed with an explicit error, never a hang.
    const POLICY_VM_CONFIG: VmConfig = VmConfig {
        max_memory_bytes: 64 * 1024 * 1024,
        max_tables: 100,
        max_instances: 10,
        fuel_per_call: 1u64 << 35,
        epoch_deadline: u64::MAX,
        call_timeout: Duration::from_secs(5),
        watchdog_tick: Duration::from_millis(10),
    };

    /// A policy plugin that hosts Luau source in the Wasm no-effect path.
    ///
    /// The policy source must define the global `kb_hot(candidate_json) ->
    /// decision_json`:
    ///
    /// ```json
    /// {"role":"ModelContext","content":"<base64>","replay_relevant":true,
    ///  "sensitivity":"test","media":"text/plain"}
    /// ```
    ///
    /// returning one of:
    ///
    /// ```json
    /// {"decision":"store"}
    /// {"decision":"transform","bytes":"<base64>"}
    /// {"decision":"drop","reason":"..."}
    /// {"decision":"reject","reason":"..."}
    /// ```
    ///
    /// `content`/`bytes` are base64 (arbitrary candidate bytes round-trip
    /// exactly); `role` is the [`CandidateRole`] variant name; `sensitivity`
    /// and `media` are `null` when absent. Unknown decisions, missing
    /// `reason`/`bytes`, and unparseable JSON are explicit
    /// [`PolicyError::Plugin`]s.
    ///
    /// Bounded: the gate's phase-1 ceiling runs before any plugin code, and
    /// [`MAX_WASM_CONTENT_BYTES`] bounds one call's content. The guest runs
    /// under [`POLICY_VM_CONFIG`]: a finite fuel budget (the deterministic
    /// interrupt — ~4x the measured cost of a max-size redaction call), an
    /// effectively-off epoch deadline (the vm's absolute-deadline API makes
    /// any finite value trip every call once the watchdog counter passes
    /// it), and the 5 s call timeout, 64 MiB memory ceiling, and 1 MiB guest
    /// scratch as backstops. One instance is kept per plugin (deterministic
    /// `kb_hot` cache); a call that traps or trips a limit is replaced with a
    /// fresh instance so the failure does not wedge the plugin — the error is
    /// still returned.
    pub struct WasmPolicyPlugin {
        vm: Vm,
        compiled: CompiledModule,
        instance: Mutex<Instance>,
        label: &'static str,
    }

    impl WasmPolicyPlugin {
        /// Compile `source` and instantiate it with [`DenyAllHost`] under
        /// [`POLICY_VM_CONFIG`]. `label` is the stable identifier
        /// [`name()`](PolicyPlugin::name) reports — conventionally
        /// `"wasm:<policy-name>"`.
        ///
        /// Failures (guest wasm absent, Luau compile error, missing `kb_hot`,
        /// instantiation limits) are explicit [`PolicyError`]s, never panics.
        pub fn new(source: &str, label: &'static str) -> Result<Self, PolicyError> {
            let vm = Vm::load(POLICY_VM_CONFIG).map_err(guest_error)?;
            let compiled = vm.compile(source).map_err(guest_error)?;
            let instance = vm
                .instantiate(&compiled, GENERATION_TOKEN, Arc::new(DenyAllHost))
                .map_err(guest_error)?;
            Ok(Self {
                vm,
                compiled,
                instance: Mutex::new(instance),
                label,
            })
        }

        /// A fresh instance of the compiled policy, replacing one interrupted
        /// by a trap or limit (a trapped instance is not reused; see the
        /// kanbei-vm tests).
        fn respawn(&self) -> Result<Instance, PolicyError> {
            self.vm
                .instantiate(&self.compiled, GENERATION_TOKEN, Arc::new(DenyAllHost))
                .map_err(guest_error)
        }
    }

    impl PolicyPlugin for WasmPolicyPlugin {
        fn decide(&self, candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            if candidate.content.len() > MAX_WASM_CONTENT_BYTES {
                return Err(PolicyError::Plugin(format!(
                    "candidate content {} bytes exceeds the wasm policy bound \
                     ({MAX_WASM_CONTENT_BYTES} bytes; guest scratch is 1 MiB)",
                    candidate.content.len()
                )));
            }

            let args = candidate_json(candidate);
            let mut instance = self
                .instance
                .lock()
                .map_err(|_| PolicyError::Plugin("policy instance lock poisoned".into()))?;
            let result = match instance.call_json("kb_hot", &args) {
                Ok(result) => result,
                Err(e) => {
                    let fresh = self.respawn()?;
                    *instance = fresh;
                    return Err(PolicyError::Plugin(format!("kb_hot call failed: {e}")));
                }
            };
            drop(instance);
            parse_decision(&result)
        }

        fn name(&self) -> &'static str {
            self.label
        }

        fn is_no_effect(&self) -> bool {
            // Enforced by construction: DenyAllHost is the only host, so no
            // capability import can succeed.
            true
        }
    }

    /// Candidate as the bounded JSON value the guest sees. Infallible: every
    /// field serializes, and `content` is bounded by the caller's checks.
    fn candidate_json(candidate: &Candidate) -> String {
        let role = match candidate.role {
            CandidateRole::ModelContext => "ModelContext",
            CandidateRole::ToolOutput => "ToolOutput",
            CandidateRole::UserInput => "UserInput",
            CandidateRole::Internal => "Internal",
        };
        let content = base64::engine::general_purpose::STANDARD.encode(&candidate.content);
        serde_json::to_string(&CandidateJson {
            role,
            content,
            replay_relevant: candidate.replay_relevant,
            sensitivity: candidate.sensitivity.as_deref(),
            media: candidate.media.as_deref(),
        })
        .expect("candidate JSON is always serializable")
    }

    #[derive(Debug, Serialize)]
    struct CandidateJson<'a> {
        role: &'static str,
        content: String,
        replay_relevant: bool,
        sensitivity: Option<&'a str>,
        media: Option<&'a str>,
    }

    /// The guest's decision JSON. Missing optional fields decode as `None`;
    /// unknown extra fields are ignored.
    #[derive(Debug, Deserialize)]
    struct DecisionJson {
        decision: String,
        bytes: Option<String>,
        reason: Option<String>,
    }

    fn parse_decision(result: &str) -> Result<RetentionDecision, PolicyError> {
        let d: DecisionJson = serde_json::from_str(result)
            .map_err(|e| PolicyError::Plugin(format!("invalid decision JSON: {e}")))?;
        match d.decision.as_str() {
            "store" => Ok(RetentionDecision::Store),
            "transform" => {
                let bytes = d.bytes.ok_or_else(|| {
                    PolicyError::Plugin("transform decision is missing \"bytes\" (base64)".into())
                })?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(bytes).map_err(
                    |e| PolicyError::Plugin(format!("transform \"bytes\" is not valid base64: {e}")),
                )?;
                Ok(RetentionDecision::Transform { bytes })
            }
            "drop" => Ok(RetentionDecision::Drop {
                reason: d.reason.ok_or_else(|| {
                    PolicyError::Plugin("drop decision is missing \"reason\"".into())
                })?,
            }),
            "reject" => Ok(RetentionDecision::RejectExecution {
                reason: d.reason.ok_or_else(|| {
                    PolicyError::Plugin("reject decision is missing \"reason\"".into())
                })?,
            }),
            other => Err(PolicyError::Plugin(format!(
                "unknown decision {other:?} (expected \"store\", \"transform\", \
                 \"drop\", or \"reject\")"
            ))),
        }
    }

    fn guest_error(e: GuestError) -> PolicyError {
        PolicyError::Plugin(format!("wasm policy runtime: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::{PatternRedactionPolicy, StoreAllPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn candidate(role: CandidateRole, content: &[u8]) -> Candidate {
        Candidate {
            role,
            content: content.to_vec(),
            replay_relevant: true,
            sensitivity: None,
            media: None,
        }
    }

    struct DropPlugin {
        reason: &'static str,
    }

    impl PolicyPlugin for DropPlugin {
        fn decide(&self, _candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::Drop {
                reason: self.reason.into(),
            })
        }

        fn name(&self) -> &'static str {
            "drop"
        }
    }

    struct SpyPlugin {
        calls: AtomicUsize,
        fail: bool,
    }

    impl PolicyPlugin for SpyPlugin {
        fn decide(&self, _candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(PolicyError::Plugin("boom".into()))
            } else {
                Ok(RetentionDecision::Store)
            }
        }

        fn name(&self) -> &'static str {
            "spy"
        }
    }

    struct RejectPlugin {
        reason: &'static str,
    }

    impl PolicyPlugin for RejectPlugin {
        fn decide(&self, _candidate: &Candidate) -> Result<RetentionDecision, PolicyError> {
            Ok(RetentionDecision::RejectExecution {
                reason: self.reason.into(),
            })
        }

        fn name(&self) -> &'static str {
            "reject"
        }
    }

    #[test]
    fn store_all_stores_original_bytes() {
        let gate = RetentionGate::new(Arc::new(StoreAllPolicy));
        let content = b"keep me as-is".to_vec();
        let admission = gate
            .admit(candidate(CandidateRole::ModelContext, &content))
            .unwrap();
        assert_eq!(
            admission,
            Admission::Stored {
                bytes: content.clone()
            }
        );
    }

    #[test]
    fn pattern_redaction_removes_secret() {
        let policy = PatternRedactionPolicy::new(
            vec!["secret-[0-9]+".into(), "token=[a-z]+".into()],
            "[REDACTED]".into(),
        )
        .unwrap();
        let gate = RetentionGate::new(Arc::new(policy));
        let admission = gate
            .admit(candidate(
                CandidateRole::ToolOutput,
                b"secret-42 and token=abc and secret-7".as_slice(),
            ))
            .unwrap();
        assert_eq!(
            admission,
            Admission::Stored {
                bytes: b"[REDACTED] and [REDACTED] and [REDACTED]".to_vec()
            }
        );
    }

    #[test]
    fn pattern_redaction_leaves_non_matching_content_unchanged() {
        let policy =
            PatternRedactionPolicy::new(vec!["secret-[0-9]+".into()], "[REDACTED]".into()).unwrap();
        let gate = RetentionGate::new(Arc::new(policy));
        let content = b"nothing to see here".to_vec();
        let admission = gate
            .admit(candidate(CandidateRole::ToolOutput, &content))
            .unwrap();
        assert_eq!(
            admission,
            Admission::Stored {
                bytes: content.clone()
            }
        );
    }

    #[test]
    fn invalid_pattern_is_rejected() {
        let err = PatternRedactionPolicy::new(vec!["(unclosed".into()], "[REDACTED]".into())
            .unwrap_err();
        assert!(matches!(err, PolicyError::InvalidPattern(_)));
        let msg = err.to_string();
        assert!(msg.contains("(unclosed"), "message should name the pattern: {msg}");
    }

    #[test]
    fn oversized_is_rejected_regardless_of_plugin() {
        // A rejecting plugin: without the size gate this would be Rejected.
        let gate = RetentionGate::new(Arc::new(RejectPlugin { reason: "nope" }))
            .with_max_candidate_bytes(4);
        let err = gate
            .admit(candidate(CandidateRole::ModelContext, b"12345"))
            .unwrap_err();
        match err {
            PolicyError::Oversized { bytes, limit } => {
                assert_eq!(bytes, 5);
                assert_eq!(limit, 4);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[test]
    fn drop_on_replay_relevant_yields_non_resumable_boundary() {
        let gate = RetentionGate::new(Arc::new(DropPlugin { reason: "forget it" }));
        let mut c = candidate(CandidateRole::ModelContext, b"model-influential");
        c.replay_relevant = true;
        let admission = gate.admit(c).unwrap();
        assert_eq!(
            admission,
            Admission::NonResumableBoundary {
                reason: "forget it".into()
            }
        );
        assert_eq!(
            gate.boundary_fact(&admission),
            Some(BoundaryFact {
                kind: BoundaryKind::NonResumable,
                reason: "forget it".into(),
                replay_relevant: true,
            })
        );
    }

    #[test]
    fn internal_role_drop_is_plain_dropped() {
        let gate = RetentionGate::new(Arc::new(DropPlugin { reason: "internal" }));
        // Even a mis-set bit cannot make Internal replay-relevant.
        let mut c = candidate(CandidateRole::Internal, b"bookkeeping");
        c.replay_relevant = true;
        let admission = gate.admit(c).unwrap();
        assert_eq!(
            admission,
            Admission::Dropped {
                reason: "internal".into()
            }
        );
        assert_eq!(gate.boundary_fact(&admission), None);
    }

    #[test]
    fn replay_default_is_conservative() {
        let gate = RetentionGate::new(Arc::new(StoreAllPolicy));
        // Model-context roles default to replay-relevant...
        assert!(gate.replay_relevant(CandidateRole::ModelContext, None));
        assert!(gate.replay_relevant(CandidateRole::ToolOutput, None));
        assert!(gate.replay_relevant(CandidateRole::UserInput, None));
        // ...unless a kernel-validated tool manifest declares otherwise...
        assert!(!gate.replay_relevant(CandidateRole::ModelContext, Some(false)));
        assert!(!gate.replay_relevant(CandidateRole::ToolOutput, Some(false)));
        // ...and Internal is never replay-relevant.
        assert!(!gate.replay_relevant(CandidateRole::Internal, None));
        assert!(!gate.replay_relevant(CandidateRole::Internal, Some(true)));
    }

    #[test]
    fn reject_execution_passes_through() {
        let gate = RetentionGate::new(Arc::new(RejectPlugin { reason: "policy says no" }));
        let admission = gate
            .admit(candidate(CandidateRole::ModelContext, b"anything"))
            .unwrap();
        assert_eq!(
            admission,
            Admission::Rejected {
                reason: "policy says no".into()
            }
        );
        assert_eq!(
            gate.boundary_fact(&admission),
            Some(BoundaryFact {
                kind: BoundaryKind::Rejected,
                reason: "policy says no".into(),
                replay_relevant: false,
            })
        );
    }

    #[test]
    fn oversized_candidate_never_reaches_plugin() {
        let spy = Arc::new(SpyPlugin {
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let gate = RetentionGate::new(spy.clone()).with_max_candidate_bytes(2);
        let err = gate
            .admit(candidate(CandidateRole::ModelContext, b"toolong"))
            .unwrap_err();
        assert!(matches!(err, PolicyError::Oversized { .. }));
        assert_eq!(spy.calls.load(Ordering::SeqCst), 0, "plugin must not run");
    }

    #[test]
    fn plugin_error_includes_plugin_name() {
        let spy = Arc::new(SpyPlugin {
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let gate = RetentionGate::new(spy.clone());
        let err = gate
            .admit(candidate(CandidateRole::ModelContext, b"data"))
            .unwrap_err();
        match err {
            PolicyError::Plugin(msg) => assert!(msg.contains("spy"), "message: {msg}"),
            other => panic!("expected Plugin, got {other:?}"),
        }
    }

    #[test]
    fn store_all_policy_name() {
        assert_eq!(StoreAllPolicy.name(), "store-all");
    }

    #[test]
    fn manifest_declared_non_relevant_drop_is_plain_dropped() {
        let gate = RetentionGate::new(Arc::new(DropPlugin { reason: "declared" }));
        let mut c = candidate(CandidateRole::ToolOutput, b"regenerable");
        c.replay_relevant = false;
        let admission = gate.admit(c).unwrap();
        assert_eq!(
            admission,
            Admission::Dropped {
                reason: "declared".into()
            }
        );
    }
}
