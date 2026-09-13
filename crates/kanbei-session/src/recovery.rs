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
/// registry's `created_session`. None = the dir carries no session identity
/// (import then opens with a fresh id). The session id is not part of the
/// layout; these are the markers a session leaves behind.
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
