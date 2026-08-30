//! Integration tests for the no-effect Wasm policy runtime (R-28/D-S3):
//! Luau policy sources hosted by kanbei-vm behind the empty capability
//! import set ([`DenyAllHost`]).
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from the
//! workspace root first; without the embedded guest wasm every test prints
//! `skip:` and passes (mirrors kanbei-vm's test skip pattern).

use std::sync::Arc;

use kanbei_policy::wasm::{MAX_WASM_CONTENT_BYTES, WasmPolicyPlugin};
use kanbei_policy::{
    Admission, Candidate, CandidateRole, PolicyError, PolicyPlugin, RetentionGate,
};

/// Skip when the guest wasm is absent (see kanbei-vm build.rs).
fn load_plugin(source: &str, label: &'static str) -> Option<WasmPolicyPlugin> {
    match WasmPolicyPlugin::new(source, label) {
        Ok(plugin) => Some(plugin),
        Err(PolicyError::Plugin(msg)) if msg.contains("guest wasm not built") => {
            eprintln!("skip: guest wasm not built (see kanbei-vm build.rs)");
            None
        }
        Err(e) => panic!("WasmPolicyPlugin::new failed: {e}"),
    }
}

fn candidate(role: CandidateRole, content: &[u8]) -> Candidate {
    Candidate {
        role,
        content: content.to_vec(),
        replay_relevant: true,
        sensitivity: Some("test".into()),
        media: Some("text/plain".into()),
    }
}

const STORE_ALL: &str = r#"
function kb_hot(c)
    assert(c.content ~= nil)
    assert(c.replay_relevant == true)
    return { decision = "store" }
end
"#;

/// Decodes and re-encodes base64 so the policy redacts the real bytes, not
/// their encoding. The guest runtime has no base64 primitive; the codec is
/// fixture code, not part of the runtime.
const REDACT: &str = r#"
local B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

local function b64decode(s)
    local out = {}
    local n, bits = 0, 0
    for i = 1, #s do
        local c = s:sub(i, i)
        if c ~= "=" then
            local v = string.find(B64, c, 1, true) - 1
            n = n * 64 + v
            bits = bits + 6
            if bits >= 8 then
                bits = bits - 8
                out[#out + 1] = string.char(math.floor(n / (2 ^ bits)) % 256)
                n = n % (2 ^ bits)
            end
        end
    end
    return table.concat(out)
end

local function b64encode(s)
    local out = {}
    for i = 1, #s, 3 do
        local b1, b2, b3 = s:byte(i, i + 2)
        local n = b1 * 65536 + (b2 or 0) * 256 + (b3 or 0)
        local pad = 3 - (#s - i + 1)
        out[#out + 1] = B64:sub(math.floor(n / 262144) % 64 + 1, math.floor(n / 262144) % 64 + 1)
        out[#out + 1] = B64:sub(math.floor(n / 4096) % 64 + 1, math.floor(n / 4096) % 64 + 1)
        out[#out + 1] = pad < 2 and B64:sub(math.floor(n / 64) % 64 + 1, math.floor(n / 64) % 64 + 1) or "="
        out[#out + 1] = pad < 1 and B64:sub(n % 64 + 1, n % 64 + 1) or "="
    end
    return table.concat(out)
end

function kb_hot(c)
    local text = b64decode(c.content)
    local redacted = string.gsub(text, "secret%-[0-9]+", "[REDACTED]")
    return { decision = "transform", bytes = b64encode(redacted) }
end
"#;

const DROP: &str = r#"
function kb_hot(c)
    if c.replay_relevant then
        return { decision = "drop", reason = "dropped by luau policy" }
    end
    return { decision = "drop", reason = "regenerable output" }
end
"#;

/// Calls the host on demand — must fail under DenyAllHost, never pass.
const HOST_CALL: &str = r#"
function kb_hot(c)
    if c.media == "deny" then
        kb_host_call(1, "{}")
    end
    return { decision = "store" }
end
"#;

#[test]
fn store_all_policy_admits_candidates() {
    let Some(plugin) = load_plugin(STORE_ALL, "wasm:store-all") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    for content in [&b"first candidate"[..], &b"second candidate"[..]] {
        let admission = gate
            .admit(candidate(CandidateRole::ToolOutput, content))
            .expect("store-all admits");
        assert_eq!(
            admission,
            Admission::Stored {
                bytes: content.to_vec()
            }
        );
    }
}

#[test]
fn pattern_redaction_policy_transforms_bytes() {
    let Some(plugin) = load_plugin(REDACT, "wasm:redact") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    let admission = gate
        .admit(candidate(CandidateRole::ToolOutput, b"secret-42 and token=abc"))
        .expect("redaction decides");
    assert_eq!(
        admission,
        Admission::Stored {
            bytes: b"[REDACTED] and token=abc".to_vec()
        }
    );
}

#[test]
fn drop_policy_non_replay_relevant_is_dropped() {
    let Some(plugin) = load_plugin(DROP, "wasm:drop") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    let mut c = candidate(CandidateRole::ToolOutput, b"regenerable");
    c.replay_relevant = false;
    let admission = gate.admit(c).expect("drop decides");
    assert_eq!(
        admission,
        Admission::Dropped {
            reason: "regenerable output".into()
        }
    );
}

#[test]
fn drop_policy_replay_relevant_is_non_resumable_boundary() {
    let Some(plugin) = load_plugin(DROP, "wasm:drop") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    let mut c = candidate(CandidateRole::ModelContext, b"model-influential");
    c.replay_relevant = true;
    let admission = gate.admit(c).expect("drop decides");
    assert_eq!(
        admission,
        Admission::NonResumableBoundary {
            reason: "dropped by luau policy".into()
        }
    );
}

#[test]
fn host_call_is_denied_and_fails_explicitly() {
    let Some(plugin) = load_plugin(HOST_CALL, "wasm:no-effects") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    let mut c = candidate(CandidateRole::ModelContext, b"anything");
    c.media = Some("deny".into());
    let err = gate.admit(c).expect_err("host call must fail, never pass");
    let PolicyError::Plugin(msg) = err else { panic!("expected Plugin, got {err:?}") };
    assert!(msg.contains("denied"), "message: {msg}");
    assert!(msg.contains("wasm:no-effects"), "message: {msg}");
    // The denied call does not wedge the plugin: the trapped instance is
    // replaced and a benign candidate still goes through.
    let admission = gate
        .admit(candidate(CandidateRole::ModelContext, b"benign"))
        .expect("plugin usable after denied call");
    assert_eq!(
        admission,
        Admission::Stored {
            bytes: b"benign".to_vec()
        }
    );
}

#[test]
fn invalid_luau_source_fails_construction() {
    match WasmPolicyPlugin::new("local x = = 1", "wasm:bad") {
        Err(PolicyError::Plugin(msg)) if msg.contains("guest wasm not built") => {
            eprintln!("skip: guest wasm not built (see kanbei-vm build.rs)");
        }
        Err(PolicyError::Plugin(msg)) => {
            assert!(msg.contains("compile"), "message: {msg}");
        }
        Ok(_) => panic!("invalid source must fail construction"),
        Err(other) => panic!("expected Plugin, got {other:?}"),
    }
}

#[test]
fn source_without_kb_hot_fails_construction() {
    match WasmPolicyPlugin::new("local x = 1", "wasm:no-hot") {
        Err(PolicyError::Plugin(msg)) if msg.contains("guest wasm not built") => {
            eprintln!("skip: guest wasm not built (see kanbei-vm build.rs)");
        }
        Err(PolicyError::Plugin(_)) => {}
        Ok(_) => panic!("source without kb_hot must fail construction"),
        Err(other) => panic!("expected Plugin, got {other:?}"),
    }
}

#[test]
fn same_candidate_same_decision() {
    let Some(plugin) = load_plugin(REDACT, "wasm:redact") else { return };
    let gate = RetentionGate::new(Arc::new(plugin));
    let c = candidate(CandidateRole::ToolOutput, b"secret-42");
    let first = gate.admit(c.clone()).expect("first decide");
    let second = gate.admit(c).expect("second decide");
    assert_eq!(first, second);
    assert_eq!(
        first,
        Admission::Stored {
            bytes: b"[REDACTED]".to_vec()
        }
    );
}

#[test]
fn over_bound_candidate_fails_explicitly() {
    let Some(plugin) = load_plugin(STORE_ALL, "wasm:store-all") else { return };
    // The gate's 16 MiB phase-1 ceiling passes; the wasm path's tighter
    // bound fails explicitly instead of overrunning the guest scratch.
    let gate = RetentionGate::new(Arc::new(plugin));
    let content = vec![0u8; MAX_WASM_CONTENT_BYTES + 1];
    let err = gate
        .admit(candidate(CandidateRole::ModelContext, &content))
        .expect_err("over-bound candidate must fail");
    let PolicyError::Plugin(msg) = err else { panic!("expected Plugin, got {err:?}") };
    assert!(msg.contains("exceeds the wasm policy bound"), "message: {msg}");
}

#[test]
fn unknown_decision_is_an_explicit_error() {
    let source = r#"
function kb_hot(c)
    return { decision = "maybe" }
end
"#;
    let Some(plugin) = load_plugin(source, "wasm:undecided") else { return };
    let err = plugin
        .decide(&candidate(CandidateRole::ModelContext, b"data"))
        .expect_err("unknown decision must fail");
    let PolicyError::Plugin(msg) = err else { panic!("expected Plugin, got {err:?}") };
    assert!(msg.contains("unknown decision"), "message: {msg}");
}

#[test]
fn name_and_no_effect_flags() {
    let Some(plugin) = load_plugin(STORE_ALL, "wasm:store-all") else { return };
    assert_eq!(plugin.name(), "wasm:store-all");
    assert!(plugin.is_no_effect(), "empty capability set must be declared");
}
