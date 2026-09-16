//! Recovery helpers: opening a session log, durability-queue shutdown, and identity/project recovery on import.

use crate::{SessionError};
use std::path::{Path};
use std::sync::Arc;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::Recovered;

// ---------- helpers ----------

/// `recover` errors on a missing file; a fresh dir is a valid genesis state.
/// The tier-1 mechanism lives in the kernel; this maps its error into the
/// session error surface.
pub(crate) fn recover_or_fresh(log_path: &Path) -> Result<Recovered, SessionError> {
    kanbei_kernel::recovery::recover_or_fresh(log_path).map_err(|e| match e {
        kanbei_kernel::recovery::RecoveryError::NotAFile(p) => {
            SessionError::InvalidInput(format!("log path is not a file: {}", p.display()))
        }
        kanbei_kernel::recovery::RecoveryError::Log(e) => SessionError::Log(e),
        kanbei_kernel::recovery::RecoveryError::Io(e) => SessionError::Io(e),
    })
}

/// Decode one canonical log line for a load-bearing recovery scan. `Ok(None)`
/// means the line is not one of `kinds`; a line that *declares* one of `kinds`
/// but does not decode is codec drift and fails loud (decision 15) rather than
/// being silently skipped.
pub(crate) fn decode_record(
    line: &str,
    kinds: &[&str],
) -> Result<Option<Envelope>, SessionError> {
    match Envelope::from_line(line) {
        Ok(env) => Ok(kinds.contains(&env.kind.as_str()).then_some(env)),
        Err(e) => match declared_kind(line).as_deref() {
            Some(k) if kinds.contains(&k) => Err(SessionError::CorruptRecord(format!(
                "{k} record does not decode: {e}"
            ))),
            _ => Ok(None),
        },
    }
}

/// The `kind` a raw canonical line declares, if it is readable. Only consulted
/// to decide whether an undecodable line belonged to a load-bearing kind.
fn declared_kind(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("kind")?
        .as_str()
        .map(str::to_owned)
}

/// Best-effort worker cleanup on a failed open, when no other Arc clones
/// exist. Only reachable while returning an error, so a secondary shutdown
/// failure is not propagated.
pub(crate) fn shutdown_queue(queue: Arc<DurabilityQueue>) {
    if let Ok(queue) = Arc::try_unwrap(queue) {
        let _ = queue.shutdown();
    }
}

/// The session id an imported dir carries, if any: the first canonical
/// identity marker, in order — a `memory_proposal` owner principal on the
/// session log, a memory transition's `origin_session`, or the project
/// registry's `created_session` (in whichever form the registry has: the
/// append-log stream, else the plain JSONL). None = the dir carries no session
/// identity (import then opens with a fresh id). The session id is not part of
/// the layout; these are the markers a session leaves behind.
pub(crate) fn recover_session_id(source_dir: &Path) -> Result<Option<Id128>, SessionError> {
    let mut found: Option<Id128> = None;
    let log_path = source_dir.join("log.zst");
    kanbei_log::for_each_frame(&log_path, |info| {
        if found.is_some() {
            return;
        }
        for line in &info.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "memory_proposal"
                && let Some(id) = env
                    .payload
                    .get("owner")
                    .and_then(|o| o.get("session"))
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse().ok())
            {
                found = Some(id);
                return;
            }
        }
    })?;
    if found.is_none() {
        let lifetime = source_dir
            .join("memory")
            .join("lifetime")
            .join("transitions.jsonl.zst");
        if lifetime.is_file() {
            kanbei_log::for_each_frame(&lifetime, |info| {
                if found.is_some() {
                    return;
                }
                for line in &info.events {
                    if let Some(id) = Envelope::from_line(line)
                        .ok()
                        .and_then(|env| {
                            env.payload
                                .get("origin_session")
                                .and_then(|s| s.as_str())
                                .and_then(|s| s.parse().ok())
                        })
                    {
                        found = Some(id);
                        return;
                    }
                }
            })?;
        }
    }
    if found.is_none() {
        let projects = source_dir.join("memory").join("projects");
        if projects.is_dir() {
            for entry in std::fs::read_dir(&projects)? {
                if found.is_some() {
                    break;
                }
                let scope_log = entry?.path().join("transitions.jsonl.zst");
                if !scope_log.is_file() {
                    continue;
                }
                kanbei_log::for_each_frame(&scope_log, |info| {
                    if found.is_some() {
                        return;
                    }
                    for line in &info.events {
                        if let Some(id) = Envelope::from_line(line)
                            .ok()
                            .and_then(|env| {
                                env.payload
                                    .get("origin_session")
                                    .and_then(|s| s.as_str())
                                    .and_then(|s| s.parse().ok())
                            })
                        {
                            found = Some(id);
                            return;
                        }
                    }
                })?;
            }
        }
    }
    if found.is_none() {
        let registry_log = source_dir
            .join("memory")
            .join("projects")
            .join("events.jsonl.zst");
        if registry_log.is_file() {
            kanbei_log::for_each_frame(&registry_log, |info| {
                if found.is_some() {
                    return;
                }
                for line in &info.events {
                    if let Some(id) = kanbei_memory::entry_of_record(line)
                        .map(|entry| entry.created_session)
                    {
                        found = Some(id);
                        return;
                    }
                }
            })?;
        }
    }
    if found.is_none() {
        let registry = source_dir.join("memory").join("projects.jsonl");
        if let Ok(text) = std::fs::read_to_string(&registry) {
            for line in text.lines() {
                if found.is_some() {
                    break;
                }
                if let Ok(entry) = serde_json::from_str::<kanbei_memory::ProjectEntry>(line) {
                    found = Some(entry.created_session);
                }
            }
        }
    }
    Ok(found)
}

/// The project bound by the imported session's log (`project_bound` fact),
/// so the project memory actor wires up like the source.
pub(crate) fn recover_bound_project(source_dir: &Path) -> Result<Option<Id128>, SessionError> {
    let mut found: Option<Id128> = None;
    let log_path = source_dir.join("log.zst");
    kanbei_log::for_each_frame(&log_path, |info| {
        if found.is_some() {
            return;
        }
        for line in &info.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "project_bound"
                && let Some(id) = env
                    .payload
                    .get("project_id")
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.parse().ok())
            {
                found = Some(id);
                return;
            }
        }
    })?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::queue::DurabilityQueue;
    use kanbei_log::{AppendLog, Profile};

    #[test]
    fn malformed_load_bearing_envelope_fails_loud() {
        // Declares a load-bearing kind but the envelope does not decode
        // (`refs` is not the digest array the record schema requires).
        let line = r#"{"env":1,"seq":1,"evt":"e","kind":"breaker_tripped","schema":1,"payload":{},"refs":"nope"}"#;
        let err = decode_record(line, &["breaker_tripped"]).unwrap_err();
        assert!(matches!(err, SessionError::CorruptRecord(_)));
    }

    #[test]
    fn malformed_unrelated_envelope_is_skipped() {
        let line = r#"{"env":1,"seq":1,"evt":"e","kind":"tool_outcome","schema":1,"payload":{},"refs":"nope"}"#;
        assert!(decode_record(line, &["breaker_tripped"]).unwrap().is_none());
    }

    #[test]
    fn well_formed_other_kind_is_none() {
        let line = r#"{"env":1,"seq":1,"evt":"e","kind":"tool_outcome","schema":1,"payload":{},"refs":[],"snapshot":null}"#;
        assert!(decode_record(line, &["breaker_tripped"]).unwrap().is_none());
    }

    /// Writes `envelope` as a single frame at `path`, creating its parents.
    fn write_one_frame(path: &Path, stream: &str, envelope: Envelope) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let queue = Arc::new(DurabilityQueue::start("test-recovery-frame"));
        {
            let mut log = AppendLog::open(path, stream, Arc::clone(&queue)).unwrap();
            log.append(&[envelope], Profile::Strict).unwrap();
        }
        Arc::try_unwrap(queue)
            .ok()
            .expect("the test holds the only queue handle")
            .shutdown()
            .unwrap();
    }

    fn envelope(seq: u64, kind: &str, payload: serde_json::Value) -> Envelope {
        Envelope {
            env: 1,
            seq,
            evt: format!("{kind}:{seq}"),
            kind: kind.into(),
            payload_schema: 1,
            payload,
            refs: Vec::new(),
            snapshot: None,
        }
    }

    /// (e) The registry's `created_session` is still a recovery marker when the
    /// registry is the append-log stream.
    #[test]
    fn registry_stream_yields_created_session() {
        let dir = std::env::temp_dir().join(format!(
            "kb-recovery-registry-{}-{}",
            std::process::id(),
            Id128::generate()
        ));
        let session = Id128::generate();
        let project = Id128::generate();
        let entry = kanbei_memory::ProjectEntry {
            schema: kanbei_memory::PROJECT_ENTRY_SCHEMA,
            project_id: project,
            name: "default".into(),
            dir: format!("projects/{project}"),
            created_session: session,
            created_event: 1,
        };
        // The source is a legacy-rooted dir: its log carries no identity
        // marker, so recovery falls through to the registry stream.
        write_one_frame(
            &dir.join("log.zst"),
            "session",
            envelope(1, "probe", serde_json::json!({})),
        );
        write_one_frame(
            &dir.join("memory").join("projects").join("events.jsonl.zst"),
            kanbei_memory::PROJECTS_STREAM,
            envelope(
                1,
                "project_registered",
                serde_json::to_value(entry).unwrap(),
            ),
        );
        assert_eq!(recover_session_id(&dir).unwrap(), Some(session));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
