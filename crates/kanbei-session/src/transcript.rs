//! The session-owned transcript projection service (decision 30).
//!
//! The session drives the projection: committed envelopes are applied on the
//! commit path, provider-stream deltas feed the in-flight partial, the driver's
//! turn end finalizes the active turn, and open replays the canonical log so a
//! resumed session rebuilds an identical view. The projection is replaceable
//! through `SessionConfig::transcript`.

use kanbei_core::envelope::Envelope;

use crate::commit::resolve_payload;
use crate::{Session, SessionError, TranscriptListener};
use kanbei_transcript::{CollapseOverrides, TranscriptProjection, TranscriptView};

impl Session {
    /// The transcript typed view, with the session-local collapse overrides
    /// applied. A function of the committed envelopes applied so far plus the
    /// session-local inputs ([`Self::finalize_transcript_turn`] and the
    /// provider-stream deltas).
    pub fn transcript_view(&self, overrides: &CollapseOverrides) -> TranscriptView {
        self.transcript.view(overrides)
    }

    /// Close the active transcript turn: the driver stopped driving and the
    /// terminal `run_outcome` (if any) is already committed, so the projection
    /// resolves the turn from that recorded fact. A no-op when no turn is
    /// active.
    pub fn finalize_transcript_turn(&mut self) {
        self.transcript.finalize_turn(None);
        self.notify_transcript();
    }

    /// Rebuild the projection from the canonical log (session open / resume;
    /// R-19 launch is always resume). One pass over the log, resolving promoted
    /// payloads so a promoted intent/outcome is not invisible to the
    /// projection; then end the replay so a leftover active turn resolves from
    /// its recorded terminal outcome (B-05: the log is the authority).
    pub(crate) fn replay_transcript(&mut self) -> Result<(), SessionError> {
        let log_path = self.log_path.clone();
        let store = &self.store;
        let transcript = &mut self.transcript;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                let mut resolved = env;
                resolved.payload = resolve_payload(store, &resolved);
                transcript.apply(&resolved);
            }
        })?;
        transcript.finish_replay();
        // The rebuild is a transcript change; a listener (the CLI) must observe
        // the replayed view rather than push it manually after open.
        self.notify_transcript();
        Ok(())
    }

    /// Fire the transcript-view observer (UI seam) with the current view.
    pub(crate) fn notify_transcript(&self) {
        fire_transcript_view(self.transcript.as_ref(), &self.transcript_listener);
    }

    /// Fire the presentation hook (UI seam), if configured: a host-command
    /// boundary where the UI should repaint before the session blocks
    /// (approval resolver) or advances (cognition step).
    pub(crate) fn fire_present_hook(&mut self) {
        if let Some(hook) = self.present_hook.clone() {
            hook(self);
        }
    }
}

/// Fire the transcript-view observer for a projection borrow. The delta path
/// holds a mutable borrow of the projection, so it cannot go through
/// [`Session::notify_transcript`] (`&self`).
pub(crate) fn fire_transcript_view(
    projection: &dyn TranscriptProjection,
    listener: &Option<TranscriptListener>,
) {
    if let Some(listener) = listener {
        listener(&projection.view(&CollapseOverrides::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use kanbei_core::envelope::Envelope;
    use kanbei_kernel::event::NewEvent;
    use kanbei_transcript::{ConversationProjection, TranscriptProjection};
    use serde_json::{Value, json};

    fn event(kind: &str, payload: Value) -> NewEvent {
        NewEvent {
            kind: kind.into(),
            payload_schema: 1,
            payload,
            objects: Vec::new(),
            refs: Vec::new(),
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kanbei-transcript-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn conn_view(dir: std::path::PathBuf) -> Session {
        Session::open(crate::SessionConfig {
            dir,
            ..Default::default()
        })
        .unwrap()
    }

    /// Acceptance (d): a replacement projection injected through
    /// `SessionConfig` is actually driven by the session.
    #[test]
    fn projection_is_replaceable_via_session_config() {
        struct Stub {
            applies: Arc<AtomicUsize>,
        }
        impl TranscriptProjection for Stub {
            fn name(&self) -> &str {
                "stub"
            }
            fn apply(&mut self, _env: &Envelope) {
                self.applies.fetch_add(1, Ordering::SeqCst);
            }
            fn finalize_turn(&mut self, _last: Option<kanbei_transcript::OutcomeClass>) {}
            fn finish_replay(&mut self) {}
            fn apply_delta(&mut self, _fragment: &str) {}
            fn end_stream(&mut self) {}
            fn view(&self, _overrides: &CollapseOverrides) -> TranscriptView {
                TranscriptView::default()
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let applies = Arc::new(AtomicUsize::new(0));
        let mut session = Session::open(crate::SessionConfig {
            dir: temp_dir("stub"),
            transcript: Some(Box::new(Stub {
                applies: Arc::clone(&applies),
            })),
            ..Default::default()
        })
        .unwrap();
        session
            .commit(
                vec![event("user_message", json!({ "text": "hi" }))],
                None,
            )
            .unwrap();
        assert!(
            applies.load(Ordering::SeqCst) >= 1,
            "the injected projection received the commit"
        );
        assert!(
            session
                .transcript_view(&CollapseOverrides::new())
                .turns
                .is_empty(),
            "the session reads the injected projection's view"
        );
    }

    /// Acceptance (b): a resumed session's replayed projection is identical to
    /// the live session's view.
    #[test]
    fn replayed_projection_matches_live_commits() {
        let dir = temp_dir("resume");
        let mut live = conn_view(dir.clone());
        live.commit(vec![event("user_message", json!({ "text": "hi" }))], None)
            .unwrap();
        live.commit(
            vec![event(
                "model_outcome",
                json!({
                    "provider": "p", "model": "m", "rendered_hash": "h",
                    "result": { "content": "done", "tool_calls": [], "finish_reason": "stop" },
                    "egress": { "input_tokens": 3, "output_tokens": 4 },
                }),
            )],
            None,
        )
        .unwrap();
        live.commit(
            vec![event(
                "run_outcome",
                json!({ "run_id": "r", "outcome": "Progress", "reason": null }),
            )],
            None,
        )
        .unwrap();
        live.finalize_transcript_turn();
        let live_view = live.transcript_view(&CollapseOverrides::new());
        live.close().unwrap();

        let resumed = conn_view(dir);
        let replayed = resumed.transcript_view(&CollapseOverrides::new());
        assert_eq!(live_view, replayed);
        assert_eq!(replayed.turns[0].response.as_deref(), Some("done"));
    }

    /// Sanity: the built-in projection is the default, and an empty session has
    /// an empty view.
    #[test]
    fn default_projection_starts_empty() {
        let session = conn_view(temp_dir("default"));
        assert_eq!(
            session.transcript_view(&CollapseOverrides::new()),
            TranscriptView::default()
        );
        assert_eq!(session.transcript.name(), ConversationProjection::new().name());
    }

    /// The listener contract: it fires on a replayed turn (session open) and on
    /// a finalized turn, and its view matches `transcript_view`.
    #[test]
    fn listener_fires_on_replay_and_finalize() {
        let views = Arc::new(std::sync::Mutex::new(Vec::<TranscriptView>::new()));
        let recorded = Arc::clone(&views);
        let listener: crate::TranscriptListener = Arc::new(move |view: &TranscriptView| {
            recorded.lock().unwrap().push(view.clone());
        });
        let mut session = Session::open(crate::SessionConfig {
            dir: temp_dir("listener"),
            transcript_listener: Some(listener),
            ..Default::default()
        })
        .unwrap();
        assert!(
            !views.lock().unwrap().is_empty(),
            "the open replay fires the listener"
        );
        session
            .commit(vec![event("user_message", json!({ "text": "hi" }))], None)
            .unwrap();
        let view = session.transcript_view(&CollapseOverrides::new());
        assert_eq!(views.lock().unwrap().last().unwrap(), &view);
        let before = views.lock().unwrap().len();
        session.finalize_transcript_turn();
        assert!(
            views.lock().unwrap().len() > before,
            "finalize fires the listener"
        );
        session.close().unwrap();
    }
}
