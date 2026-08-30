//! M2 capability model and broker.
//!
//! See docs/architecture.md "Capability model": effective authority for a tool
//! effect is the intersection `caller grant ∩ provider policy template ∩
//! policy ∩ budget` (R-14/D-02). This crate owns the model — grants, policy
//! templates, approval intents — and the broker (`check` / `attenuate` /
//! `require_approval` / `recheck`). Principal resolution is internal to the
//! kernel and is not a seam here: the broker receives the resolved
//! [`Principal`] per invocation.
//!
//! M2 scope: the model, the broker, and the approval-intent shapes. The
//! interactive approval loop and the concrete tool registry are M3; where a
//! decision needs M3 inputs (committed intent arguments, cwd/env fingerprint,
//! scope/expiry of an approval), the broker uses documented M2 defaults.
//!
//! Key decisions:
//! - **Templates are keyed by origin trust class** (R-13/D-04) for
//!   registration: `add_template` enforces monotonic growth per trust class.
//!   `check` has no trust-class input, so it applies the union of all
//!   registered templates: any template's deny wins, any template's
//!   `require_approval` gates, any template's allow permits; a verb no
//!   template mentions is denied (default-deny, R-13).
//! - **Digests are domain-separated** (R-16/D-12): the canonical JSON embeds
//!   a `domain` field (`capability-grant-v1`, `approval-v1`) so grant and
//!   object digests never collide.
//! - **Budget** counts successful `check` calls only; `recheck` verifies
//!   remaining budget but does not consume it.
//! - **Grants version** is the grant count (the M2 broker is add-only;
//!   revocation is M3 and will version the grant set explicitly).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use kanbei_core::{Digest, Id128};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// Dotted resource name in the capability vocabulary:
/// `fs.read`, `fs.write`, `process.run`, `memory.query`, `memory.propose`,
/// `service.call`, `tool.*`, `state.write`.
///
/// M2 treats resources as opaque strings matched exactly; `tool.*` wildcard
/// expansion happens in the M3 concrete tool registry.
pub type Resource = String;

/// Caller principal (R-14/D-02): every invocation carries the initiating
/// principal. `generation` is the caller module's generation; `run` is the
/// session run it originated in, if any.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Principal {
    pub session: Id128,
    pub generation: u64,
    pub run: Option<u64>,
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session:{} generation:{}", self.session, self.generation)?;
        if let Some(run) = self.run {
            write!(f, " run:{run}")?;
        }
        Ok(())
    }
}

/// A requested or granted capability: a resource plus the verbs allowed on it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Capability {
    pub resource: Resource,
    pub verbs: Vec<String>,
}

impl Capability {
    /// Builds a capability with verbs sorted and deduplicated.
    pub fn new(resource: Resource, mut verbs: Vec<String>) -> Self {
        verbs.sort();
        verbs.dedup();
        Self { resource, verbs }
    }
}

/// Origin trust class of a module (R-13/D-04). Policy templates are keyed by
/// this class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustClass {
    User,
    Workspace,
    Agent,
    Builtin,
}

/// Explicit scope of a grant or approval (R-16/D-12). `Standing` is only
/// reachable through this explicit variant and is rejected by the broker
/// without a purpose: standing approvals without scope are prohibited — the
/// scope IS the `Standing` variant plus its purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrantScope {
    Run,
    Session,
    Project,
    Standing,
}

fn scope_str(scope: GrantScope) -> &'static str {
    match scope {
        GrantScope::Run => "run",
        GrantScope::Session => "session",
        GrantScope::Project => "project",
        GrantScope::Standing => "standing",
    }
}

/// A grant of capability to a principal (R-13/D-01). Scoped by principal,
/// module generation, resource, verbs, expiry, budget, and purpose.
///
/// `grant_digest` binds every other field, domain-separated under
/// `capability-grant-v1` (R-16/D-12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub principal: Principal,
    pub module_generation: u64,
    pub capability: Capability,
    pub scope: GrantScope,
    /// Unix seconds; `None` means no expiry.
    pub expiry: Option<u64>,
    /// Remaining allowed call count; `None` means uncounted.
    pub budget: Option<u64>,
    pub purpose: Option<String>,
    pub policy_version: u64,
    pub grant_digest: Digest,
}

impl Grant {
    /// Recomputes the grant digest over the canonical JSON of every field
    /// except the digest itself.
    pub fn derive_digest(&self) -> Digest {
        digest_of(&self.canonical_json())
    }

    /// True when the stored `grant_digest` matches a fresh derivation.
    pub fn validate(&self) -> bool {
        self.derive_digest() == self.grant_digest
    }

    /// Canonical JSON shape (pub(crate) so tests can exercise domain
    /// separation). Field order is fixed; serde_json maps serialize keys in
    /// sorted order, so the bytes are stable.
    pub(crate) fn canonical_json(&self) -> Value {
        json!({
            "domain": "capability-grant-v1",
            "principal": principal_json(&self.principal),
            "module_generation": self.module_generation,
            "capability": {
                "resource": self.capability.resource,
                "verbs": self.capability.verbs,
            },
            "scope": scope_str(self.scope),
            "expiry": self.expiry,
            "budget": self.budget,
            "purpose": self.purpose,
            "policy_version": self.policy_version,
        })
    }
}

/// The committed approval intent (R-16/D-12): binds tool ModuleId+generation,
/// action type, canonicalized arguments, and cwd/env fingerprint,
/// domain-separated under `approval-v1`. Approvals carry explicit scope and
/// expiry; standing approvals without scope are prohibited.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalIntent {
    pub digest: Digest,
    pub principal: Principal,
    pub module_generation: u64,
    /// Action type — the dotted resource being invoked (e.g. `process.run`).
    pub action: String,
    /// Canonicalized arguments (object keys sorted recursively).
    pub args: Value,
    pub cwd_env_fingerprint: Option<String>,
    pub scope: GrantScope,
    /// Unix seconds; `None` means no expiry.
    pub expiry: Option<u64>,
}

impl ApprovalIntent {
    /// Recomputes the intent digest over the canonical JSON of every field
    /// except the digest itself.
    pub fn derive_digest(&self) -> Digest {
        digest_of(&self.canonical_json())
    }

    /// True when the stored `digest` matches a fresh derivation.
    pub fn validate(&self) -> bool {
        self.derive_digest() == self.digest
    }

    pub(crate) fn canonical_json(&self) -> Value {
        json!({
            "domain": "approval-v1",
            "principal": principal_json(&self.principal),
            "module_generation": self.module_generation,
            "action": self.action,
            "args": canonicalize(self.args.clone()),
            "cwd_env_fingerprint": self.cwd_env_fingerprint,
            "scope": scope_str(self.scope),
            "expiry": self.expiry,
        })
    }
}

/// Policy template keyed by origin trust class (R-13/D-04). `allow`/`deny`/
/// `require_approval` are capability lists; deny wins over allow and over
/// approval gating.
///
/// `monotonic` marks the template's terms as append-only guards: replacing a
/// monotonic template with a same-trust-class template that removes any
/// allow/deny/require_approval entry is rejected by the broker
/// (`NonMonotonicPolicy`). Non-monotonic templates can be replaced freely —
/// the bootstrap path for provisional policy that is later tightened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyTemplate {
    pub trust_class: TrustClass,
    pub allow: Vec<Capability>,
    pub deny: Vec<Capability>,
    pub require_approval: Vec<Capability>,
    pub monotonic: bool,
    pub version: u64,
}

/// Result of a successful `Broker::check`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The intersection of the requested, granted, and policy-allowed verbs.
    pub cap: Capability,
    /// Budget calls left after this check; `None` for uncounted grants.
    pub remaining_budget: Option<u64>,
    /// True when some requested verb is approval-gated (still checkable).
    pub requires_approval: bool,
}

/// Errors from the capability broker. Each variant names the resource, verb,
/// and/or principal involved.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrokerError {
    #[error("non-monotonic policy for trust class {trust_class:?}: replacement removes an allow/deny/require_approval entry")]
    NonMonotonicPolicy { trust_class: TrustClass },
    #[error("no grant for {resource} under {principal}")]
    NoGrant { resource: Resource, principal: Principal },
    #[error("denied {resource}/{verb} for {principal}")]
    Denied { resource: Resource, verb: String, principal: Principal },
    #[error("grant for {resource} under {principal} has expired")]
    Expired { resource: Resource, principal: Principal },
    #[error("budget exhausted for {resource} under {principal}")]
    BudgetExhausted { resource: Resource, principal: Principal },
    #[error("policy version mismatch: expected {expected}, actual {actual}")]
    PolicyVersionMismatch { expected: u64, actual: u64 },
    #[error("grant digest validation failed: {digest}")]
    StaleGrant { digest: Digest },
    #[error("capability {resource}/{verb} is not approval-gated")]
    NotApprovalGated { resource: Resource, verb: String },
    #[error("approval intent digest validation failed: {digest}")]
    StaleIntent { digest: Digest },
    #[error("grant set version mismatch: expected {expected}, actual {actual}")]
    GrantsVersionMismatch { expected: u64, actual: u64 },
    #[error("standing grant for {resource} requires a purpose")]
    StandingRequiresPurpose { resource: Resource },
}

/// The M2 capability broker.
#[derive(Default)]
pub struct Broker {
    pub templates: Vec<PolicyTemplate>,
    pub grants: Vec<Grant>,
    /// Budget units consumed per grant digest, so `check(&self)` can account
    /// for call-count budgets without mutating the grant set.
    spent: RefCell<HashMap<Digest, u64>>,
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a policy template. A same-trust-class template replaces the
    /// previous one, but only if the previous one is not a monotonic guard
    /// whose allow/deny/require_approval sets the replacement shrinks.
    pub fn add_template(&mut self, t: PolicyTemplate) -> Result<(), BrokerError> {
        let position = self.templates.iter().position(|e| e.trust_class == t.trust_class);
        if let Some(pos) = position {
            let existing = &self.templates[pos];
            if existing.monotonic && !template_grows(&t, existing) {
                return Err(BrokerError::NonMonotonicPolicy { trust_class: t.trust_class });
            }
            self.templates[pos] = t;
        } else {
            self.templates.push(t);
        }
        Ok(())
    }

    /// Registers a grant. Rejects tampered digests, already-expired grants,
    /// and standing grants without a purpose.
    pub fn add_grant(&mut self, g: Grant) -> Result<(), BrokerError> {
        if !g.validate() {
            return Err(BrokerError::StaleGrant { digest: g.grant_digest });
        }
        if g.expiry.is_some_and(|e| e <= now_secs()) {
            return Err(BrokerError::Expired { resource: g.capability.resource.clone(), principal: g.principal });
        }
        if g.scope == GrantScope::Standing && g.purpose.is_none() {
            return Err(BrokerError::StandingRequiresPurpose { resource: g.capability.resource.clone() });
        }
        self.grants.push(g);
        Ok(())
    }

    /// The intersection check: caller grant ∩ policy templates ∩ budget.
    ///
    /// Guard order: no grant → `NoGrant`; tampered grant → `StaleGrant`;
    /// expired → `Expired`; budget spent → `BudgetExhausted`; policy version
    /// drift → `PolicyVersionMismatch`; then per requested verb: any
    /// template's deny → `Denied`, verb not covered by the grant → `NoGrant`,
    /// verb not allowed by any template → `Denied` (default-deny).
    ///
    /// Budget is consumed only when the check succeeds; `remaining_budget` is
    /// what remains after this call.
    pub fn check(&self, principal: &Principal, want: &Capability, policy_version: u64) -> Result<Effective, BrokerError> {
        let grant = self
            .find_grant(principal, &want.resource)
            .ok_or_else(|| BrokerError::NoGrant { resource: want.resource.clone(), principal: principal.clone() })?;
        if !grant.validate() {
            return Err(BrokerError::StaleGrant { digest: grant.grant_digest });
        }
        if grant.expiry.is_some_and(|e| e <= now_secs()) {
            return Err(BrokerError::Expired { resource: want.resource.clone(), principal: principal.clone() });
        }
        let spent = self.spent.borrow().get(&grant.grant_digest).copied().unwrap_or(0);
        let remaining = grant.budget.map(|b| b.saturating_sub(spent));
        if remaining == Some(0) {
            return Err(BrokerError::BudgetExhausted { resource: want.resource.clone(), principal: principal.clone() });
        }
        let actual_policy = self.policy_version();
        if policy_version != actual_policy {
            return Err(BrokerError::PolicyVersionMismatch { expected: policy_version, actual: actual_policy });
        }

        // Policy guards run before grant coverage: deny wins over everything.
        for verb in &want.verbs {
            if self.templates.iter().any(|t| contains(&t.deny, &want.resource, verb)) {
                return Err(BrokerError::Denied { resource: want.resource.clone(), verb: verb.clone(), principal: principal.clone() });
            }
        }

        let mut effective_verbs = Vec::new();
        let mut requires_approval = false;
        for verb in &want.verbs {
            if !grant.capability.verbs.contains(verb) {
                return Err(BrokerError::NoGrant { resource: want.resource.clone(), principal: principal.clone() });
            }
            if !self.templates.iter().any(|t| contains(&t.allow, &want.resource, verb)) {
                return Err(BrokerError::Denied { resource: want.resource.clone(), verb: verb.clone(), principal: principal.clone() });
            }
            effective_verbs.push(verb.clone());
            if self.templates.iter().any(|t| contains(&t.require_approval, &want.resource, verb)) {
                requires_approval = true;
            }
        }

        if grant.budget.is_some() {
            self.spent.borrow_mut().insert(grant.grant_digest, spent + 1);
        }
        Ok(Effective {
            cap: Capability::new(want.resource.clone(), effective_verbs),
            remaining_budget: remaining.map(|r| r - 1),
            requires_approval,
        })
    }

    /// Pure attenuation: returns `base` narrowed by `drop_verbs`. Dropping a
    /// verb the base does not have is a no-op, so attenuation can never widen.
    pub fn attenuate(&self, base: &Capability, drop_verbs: &[String]) -> Capability {
        let kept = base
            .verbs
            .iter()
            .filter(|v| !drop_verbs.contains(v))
            .cloned()
            .collect();
        Capability::new(base.resource.clone(), kept)
    }

    /// Builds the approval intent for an approval-gated capability.
    ///
    /// Gate-check semantics: gated when ANY requested verb appears in some
    /// template's `require_approval` list; the intent then covers the whole
    /// capability (conservative). Not in any list → `NotApprovalGated`.
    ///
    /// M2 defaults (the M3 loop supplies the real values): args are the
    /// canonicalized empty object, `cwd_env_fingerprint` is `None`, scope is
    /// `Run`, expiry is `None`. `module_generation` is taken from the
    /// principal, matching `check`'s grant matching.
    pub fn require_approval(&self, principal: &Principal, want: &Capability) -> Result<ApprovalIntent, BrokerError> {
        let gated = want
            .verbs
            .iter()
            .any(|v| self.templates.iter().any(|t| contains(&t.require_approval, &want.resource, v)));
        if !gated {
            return Err(BrokerError::NotApprovalGated {
                resource: want.resource.clone(),
                verb: want.verbs.first().cloned().unwrap_or_default(),
            });
        }
        let mut intent = ApprovalIntent {
            digest: Digest::new(b""),
            principal: principal.clone(),
            module_generation: principal.generation,
            action: want.resource.clone(),
            args: canonicalize(json!({})),
            cwd_env_fingerprint: None,
            scope: GrantScope::Run,
            expiry: None,
        };
        intent.digest = intent.derive_digest();
        Ok(intent)
    }

    /// Dispatch-time re-verification (R-16/D-11): recomputes the intent
    /// digest (`StaleIntent` on drift), verifies the policy version and the
    /// grant-set version are unchanged, then re-runs the guards — the grant
    /// must still exist, be valid, unexpired, within budget (verified, not
    /// consumed), and not denied by any template.
    ///
    /// The intent's `action` is matched against the grant's resource; the
    /// grant's `module_generation` must equal the intent's, and the grant's
    /// principal must equal the intent's principal.
    pub fn recheck(&self, intent: &ApprovalIntent, current_grants_version: u64, policy_version: u64) -> Result<(), BrokerError> {
        if !intent.validate() {
            return Err(BrokerError::StaleIntent { digest: intent.digest });
        }
        let actual_policy = self.policy_version();
        if policy_version != actual_policy {
            return Err(BrokerError::PolicyVersionMismatch { expected: policy_version, actual: actual_policy });
        }
        let grants_version = self.grants.len() as u64;
        if current_grants_version != grants_version {
            return Err(BrokerError::GrantsVersionMismatch { expected: current_grants_version, actual: grants_version });
        }
        let grant = self
            .find_grant(&intent.principal, &intent.action)
            .ok_or_else(|| BrokerError::NoGrant { resource: intent.action.clone(), principal: intent.principal.clone() })?;
        if !grant.validate() {
            return Err(BrokerError::StaleGrant { digest: grant.grant_digest });
        }
        if grant.expiry.is_some_and(|e| e <= now_secs()) {
            return Err(BrokerError::Expired { resource: intent.action.clone(), principal: intent.principal.clone() });
        }
        let spent = self.spent.borrow().get(&grant.grant_digest).copied().unwrap_or(0);
        if grant.budget.is_some_and(|b| spent >= b) {
            return Err(BrokerError::BudgetExhausted { resource: intent.action.clone(), principal: intent.principal.clone() });
        }
        for verb in &grant.capability.verbs {
            if self.templates.iter().any(|t| contains(&t.deny, &intent.action, verb)) {
                return Err(BrokerError::Denied { resource: intent.action.clone(), verb: verb.clone(), principal: intent.principal.clone() });
            }
        }
        Ok(())
    }

    /// Grant-set version: the grant count, used by dispatch-time
    /// re-verification (R-16/D-11/C-10) to detect revocation.
    pub fn grants_version(&self) -> u64 {
        self.grants.len() as u64
    }

    /// Highest version across registered templates; 0 when none are present.
    pub fn policy_version(&self) -> u64 {
        self.templates.iter().map(|t| t.version).max().unwrap_or(0)
    }

    /// Finds the grant covering `resource` for `principal`: session and
    /// generation must match exactly; a grant with `run: None` is session-wide
    /// (matches any caller run), a run-scoped grant must match the caller's
    /// run. `module_generation` must equal the caller's generation — the
    /// caller is the tool-provider module, so the grant's module-generation
    /// pin is checked against the invoking module.
    fn find_grant<'a>(&'a self, principal: &Principal, resource: &str) -> Option<&'a Grant> {
        self.grants.iter().find(|g| {
            g.principal.session == principal.session
                && g.principal.generation == principal.generation
                && (g.principal.run.is_none() || g.principal.run == principal.run)
                && g.module_generation == principal.generation
                && g.capability.resource == resource
        })
    }
}

fn principal_json(principal: &Principal) -> Value {
    json!({
        "session": principal.session.to_string(),
        "generation": principal.generation,
        "run": principal.run,
    })
}

fn digest_of(canonical: &Value) -> Digest {
    let bytes = serde_json::to_vec(canonical).expect("serializing canonical JSON cannot fail");
    Digest::new(&bytes)
}

/// Recursively sorts object keys so equivalent documents hash identically.
fn canonicalize(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> =
                map.into_iter().map(|(k, val)| (k, canonicalize(val))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

fn contains(caps: &[Capability], resource: &str, verb: &str) -> bool {
    caps.iter()
        .any(|c| c.resource == resource && c.verbs.iter().any(|v| v == verb))
}

/// True when `new` keeps every entry of `old`: each old capability is covered
/// by a new capability with the same resource and all its verbs. Adding verbs
/// or capabilities is growth; removing any is not.
fn template_grows(new: &PolicyTemplate, old: &PolicyTemplate) -> bool {
    let keeps = |old_caps: &[Capability], new_caps: &[Capability]| {
        old_caps.iter().all(|old_c| {
            new_caps
                .iter()
                .any(|new_c| new_c.resource == old_c.resource && old_c.verbs.iter().all(|v| new_c.verbs.contains(v)))
        })
    };
    keeps(&old.allow, &new.allow) && keeps(&old.deny, &new.deny) && keeps(&old.require_approval, &new.require_approval)
}

fn now_secs() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(run: Option<u64>) -> Principal {
        Principal { session: Id128::generate(), generation: 1, run }
    }

    fn grant(p: &Principal, resource: &str, verbs: &[&str], budget: Option<u64>, expiry: Option<u64>) -> Grant {
        let mut g = Grant {
            principal: p.clone(),
            module_generation: p.generation,
            capability: Capability::new(resource.to_string(), verbs.iter().map(|s| s.to_string()).collect()),
            scope: GrantScope::Session,
            expiry,
            budget,
            purpose: None,
            policy_version: 1,
            grant_digest: Digest::new(b""),
        };
        g.grant_digest = g.derive_digest();
        g
    }

    fn template(allow: &[&str], deny: &[&str], require_approval: &[&str]) -> PolicyTemplate {
        let caps = |names: &[&str]| {
            names
                .iter()
                .map(|n| {
                    Capability::new(
                        n.to_string(),
                        vec!["read".to_string(), "write".to_string(), "invoke".to_string()],
                    )
                })
                .collect()
        };
        PolicyTemplate {
            trust_class: TrustClass::User,
            allow: caps(allow),
            deny: caps(deny),
            require_approval: caps(require_approval),
            monotonic: true,
            version: 1,
        }
    }

    #[test]
    fn intersection_deny_wins() {
        let mut broker = Broker::new();
        broker
            .add_template(template(&["fs.read", "fs.write"], &["fs.write"], &[]))
            .unwrap();
        let p = principal(None);
        broker.add_grant(grant(&p, "fs.read", &["read"], None, None)).unwrap();
        broker.add_grant(grant(&p, "fs.write", &["write"], None, None)).unwrap();

        let eff = broker.check(&p, &Capability::new("fs.read".into(), vec!["read".into()]), 1).unwrap();
        assert_eq!(eff.cap.verbs, vec!["read"]);
        assert!(!eff.requires_approval);
        assert_eq!(eff.remaining_budget, None);

        let err = broker.check(&p, &Capability::new("fs.write".into(), vec!["write".into()]), 1).unwrap_err();
        assert!(matches!(err, BrokerError::Denied { resource, verb, .. } if resource == "fs.write" && verb == "write"));
    }

    #[test]
    fn attenuation_narrows_and_never_widens() {
        let broker = Broker::new();
        let base = Capability::new("fs".into(), vec!["read".into(), "write".into()]);

        let narrowed = broker.attenuate(&base, &["write".to_string()]);
        assert_eq!(narrowed.verbs, vec!["read"]);

        // Dropping a verb the base does not have is a no-op: unchanged.
        let unchanged = broker.attenuate(&base, &["execute".to_string()]);
        assert_eq!(unchanged, base);
        // Dropping everything leaves the resource with an empty verb set.
        let empty = broker.attenuate(&base, &["read".to_string(), "write".to_string()]);
        assert!(empty.verbs.is_empty());
        assert_eq!(empty.resource, base.resource);
    }

    #[test]
    fn no_grant_errors() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(None);
        let want = Capability::new("fs.read".into(), vec!["read".into()]);

        let err = broker.check(&p, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::NoGrant { ref resource, .. } if resource == "fs.read"));

        // A grant for a different resource does not cover this check.
        let q = principal(None);
        broker.add_grant(grant(&q, "fs.write", &["write"], None, None)).unwrap();
        let err = broker.check(&p, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::NoGrant { ref resource, .. } if resource == "fs.read"));

        // A grant covering the resource but not the verb is NoGrant too.
        broker.add_grant(grant(&p, "fs.read", &["write"], None, None)).unwrap();
        let err = broker.check(&p, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::NoGrant { .. }));
    }

    #[test]
    fn policy_silence_is_default_deny() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(None);
        broker.add_grant(grant(&p, "fs.write", &["write"], None, None)).unwrap();
        let err = broker.check(&p, &Capability::new("fs.write".into(), vec!["write".into()]), 1).unwrap_err();
        assert!(matches!(err, BrokerError::Denied { resource, verb, .. } if resource == "fs.write" && verb == "write"));
    }

    #[test]
    fn expired_and_budget() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(None);
        let want = Capability::new("fs.read".into(), vec!["read".into()]);

        // add_grant rejects already-expired grants.
        let err = broker.add_grant(grant(&p, "fs.read", &["read"], None, Some(1))).unwrap_err();
        assert!(matches!(err, BrokerError::Expired { .. }));

        // An expired grant already in the broker fails check.
        broker.grants.push(grant(&p, "fs.read", &["read"], None, Some(1)));
        let err = broker.check(&p, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::Expired { ref resource, .. } if resource == "fs.read"));

        // A 2-call budget: two successful checks, then BudgetExhausted.
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        broker.add_grant(grant(&p, "fs.read", &["read"], Some(2), None)).unwrap();

        // A 2-call budget: two successful checks, then BudgetExhausted.
        broker.add_grant(grant(&p, "fs.read", &["read"], Some(2), None)).unwrap();
        let first = broker.check(&p, &want, 1).unwrap();
        assert_eq!(first.remaining_budget, Some(1));
        let second = broker.check(&p, &want, 1).unwrap();
        assert_eq!(second.remaining_budget, Some(0));
        let err = broker.check(&p, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::BudgetExhausted { ref resource, .. } if resource == "fs.read"));
    }

    #[test]
    fn policy_version_mismatch() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(None);
        broker.add_grant(grant(&p, "fs.read", &["read"], None, None)).unwrap();
        let want = Capability::new("fs.read".into(), vec!["read".into()]);

        let err = broker.check(&p, &want, 2).unwrap_err();
        assert!(matches!(err, BrokerError::PolicyVersionMismatch { expected: 2, actual: 1 }));
    }

    #[test]
    fn monotonic_template_growth() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &["fs.write"], &[])).unwrap();

        // Removing a deny from a monotonic guard is rejected.
        let err = broker.add_template(template(&["fs.read"], &[], &[])).unwrap_err();
        assert!(matches!(err, BrokerError::NonMonotonicPolicy { trust_class: TrustClass::User }));

        // Removing an allow is rejected.
        let err = broker.add_template(template(&[], &["fs.write"], &[])).unwrap_err();
        assert!(matches!(err, BrokerError::NonMonotonicPolicy { .. }));

        // Growth (superset deny, new allow) replaces in place.
        let mut grown = template(&["fs.read", "memory.query"], &["fs.write"], &[]);
        grown.version = 2;
        broker.add_template(grown).unwrap();
        assert_eq!(broker.templates.len(), 1);
        assert_eq!(broker.policy_version(), 2);

        // A non-monotonic template can be replaced by a narrower one.
        let mut broker = Broker::new();
        let mut provisional = template(&["fs.read"], &["fs.write"], &[]);
        provisional.monotonic = false;
        broker.add_template(provisional).unwrap();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        assert_eq!(broker.templates.len(), 1);
    }

    #[test]
    fn approval_intent_digest_and_recheck() {
        let mut broker = Broker::new();
        broker
            .add_template(template(&["fs.read"], &[], &["fs.read"]))
            .unwrap();
        let p = principal(None);
        broker.add_grant(grant(&p, "fs.read", &["read"], None, None)).unwrap();
        let want = Capability::new("fs.read".into(), vec!["read".into()]);

        let intent = broker.require_approval(&p, &want).unwrap();
        // Derivation is stable and matches the committed digest.
        assert!(intent.validate());
        assert_eq!(intent.derive_digest(), intent.digest);
        let clone = intent.clone();
        assert!(clone.validate());
        assert_eq!(intent.action, "fs.read");
        assert_eq!(intent.module_generation, p.generation);
        assert_eq!(intent.scope, GrantScope::Run);
        assert_eq!(intent.expiry, None);

        // Mutating args invalidates the digest.
        let mut tampered = intent.clone();
        tampered.args = json!({"path": "/etc/passwd"});
        assert!(!tampered.validate());

        // Dispatch-time re-verification passes unchanged and fails on drift.
        broker.recheck(&intent, 1, 1).unwrap();
        let err = broker.recheck(&intent, 1, 2).unwrap_err();
        assert!(matches!(err, BrokerError::PolicyVersionMismatch { expected: 2, actual: 1 }));
        let err = broker.recheck(&intent, 2, 1).unwrap_err();
        assert!(matches!(err, BrokerError::GrantsVersionMismatch { expected: 2, actual: 1 }));
        let err = broker.recheck(&tampered, 1, 1).unwrap_err();
        assert!(matches!(err, BrokerError::StaleIntent { .. }));
    }

    #[test]
    fn standing_grant_requires_purpose() {
        let mut broker = Broker::new();
        let p = principal(None);
        let mut standing = grant(&p, "fs.read", &["read"], None, None);
        standing.scope = GrantScope::Standing;
        standing.grant_digest = standing.derive_digest();
        let err = broker.add_grant(standing).unwrap_err();
        assert!(matches!(err, BrokerError::StandingRequiresPurpose { ref resource, .. } if resource == "fs.read"));

        let mut with_purpose = grant(&p, "fs.read", &["read"], None, None);
        with_purpose.scope = GrantScope::Standing;
        with_purpose.purpose = Some("user-configured scheduled cleanup".to_string());
        with_purpose.grant_digest = with_purpose.derive_digest();
        broker.add_grant(with_purpose).unwrap();
        assert_eq!(broker.grants.len(), 1);
    }

    #[test]
    fn grant_digest_domain_separation() {
        let p = principal(None);
        let g1 = grant(&p, "fs.read", &["read"], None, None);

        // Same fields, different domain prefix: different digest, and the
        // altered digest no longer validates against the capability-grant-v1
        // derivation.
        let mut v2 = g1.canonical_json();
        v2["domain"] = json!("capability-grant-v2");
        let mut g2 = g1.clone();
        g2.grant_digest = digest_of(&v2);

        assert_ne!(g1.grant_digest, g2.grant_digest);
        assert!(g1.validate());
        assert!(!g2.validate());
    }

    #[test]
    fn require_approval_gate_check() {
        let mut broker = Broker::new();
        broker
            .add_template(template(&["fs.read", "fs.write"], &[], &["fs.read"]))
            .unwrap();
        let p = principal(None);
        let want = Capability::new("fs.write".into(), vec!["write".into()]);

        let err = broker.require_approval(&p, &want).unwrap_err();
        assert!(matches!(err, BrokerError::NotApprovalGated { resource, verb, .. } if resource == "fs.write" && verb == "write"));

        // Gated capability yields a valid intent; the effective check flags it.
        let intent = broker.require_approval(&p, &Capability::new("fs.read".into(), vec!["read".into()])).unwrap();
        assert!(intent.validate());
        broker.add_grant(grant(&p, "fs.read", &["read"], None, None)).unwrap();
        let eff = broker.check(&p, &Capability::new("fs.read".into(), vec!["read".into()]), 1).unwrap();
        assert!(eff.requires_approval);
    }

    #[test]
    fn stale_grant_rejected() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(None);

        // add_grant rejects a tampered digest.
        let mut tampered = grant(&p, "fs.read", &["read"], None, None);
        tampered.grant_digest = Digest::new(b"forged");
        let err = broker.add_grant(tampered.clone()).unwrap_err();
        assert!(matches!(err, BrokerError::StaleGrant { .. }));

        // A stale grant already in the broker fails check and recheck.
        broker.grants.push(tampered);
        let err = broker.check(&p, &Capability::new("fs.read".into(), vec!["read".into()]), 1).unwrap_err();
        assert!(matches!(err, BrokerError::StaleGrant { digest } if digest == Digest::new(b"forged")));
        let mut intent = ApprovalIntent {
            digest: Digest::new(b""),
            principal: p.clone(),
            module_generation: p.generation,
            action: "fs.read".to_string(),
            args: json!({}),
            cwd_env_fingerprint: None,
            scope: GrantScope::Run,
            expiry: None,
        };
        intent.digest = intent.derive_digest();
        let err = broker.recheck(&intent, 1, 1).unwrap_err();
        assert!(matches!(err, BrokerError::StaleGrant { .. }));
    }

    #[test]
    fn capability_new_sorts_and_dedups() {
        let c = Capability::new("fs.read".into(), vec!["write".into(), "read".into(), "write".into()]);
        assert_eq!(c.verbs, vec!["read", "write"]);
    }

    #[test]
    fn run_scoped_grant_matches_only_its_run() {
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let p = principal(Some(7));
        broker.add_grant(grant(&p, "fs.read", &["read"], None, None)).unwrap();
        let want = Capability::new("fs.read".into(), vec!["read".into()]);

        broker.check(&p, &want, 1).unwrap();
        let other_run = Principal { session: p.session, generation: 1, run: Some(8) };
        let err = broker.check(&other_run, &want, 1).unwrap_err();
        assert!(matches!(err, BrokerError::NoGrant { .. }));

        // A run-less grant is session-wide and matches any caller run.
        let mut broker = Broker::new();
        broker.add_template(template(&["fs.read"], &[], &[])).unwrap();
        let q = principal(None);
        broker.add_grant(grant(&q, "fs.read", &["read"], None, None)).unwrap();
        let caller = Principal { session: q.session, generation: 1, run: Some(3) };
        broker.check(&caller, &want, 1).unwrap();
    }
}
