//! M8 wave 1: optional OTel-compatible telemetry hooks (feature `otel`).
//! Correlation spans over the canonical run/commit/checkpoint/transition
//! records and storage gauges over filesystem observations + the canonical
//! committed seq. Values derive from canonical records (M7 principle) —
//! never private session internals; storage metrics are filesystem
//! observations.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_telemetry::{AttrValue, SpanKind, StatusCode};

use crate::{BranchId, CommitReceipt, Session, SessionError};

pub(crate) fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos() as u64
}

impl Session {
    /// Open the run span at `run_start`: trace = session id, span = run id
    /// (first 8 bytes — the OTLP span-id width), start = the canonical
    /// scheduler start (RunUsage.started_at_secs).
    pub(crate) fn telemetry_open_run(&mut self, run_id: Id128) {
        let Some(t) = &self.telemetry else {
            return;
        };
        let started = self.scheduler_usage(run_id).started_at_secs;
        self.open_run_span = Some(
            t.span("run")
                .kind(SpanKind::Server)
                .trace_id(self.session_id)
                .span_id(run_id)
                .start(started.saturating_mul(1_000_000_000))
                .attr("session_id", AttrValue::Str(self.session_id.to_string()))
                .attr("run_id", AttrValue::Str(run_id.to_string()))
                .attr("started_at_secs", AttrValue::Int(started as i64)),
        );
    }

    /// Close the run span with the terminal status + usage attrs (the
    /// scheduler-accumulated usage of the canonical records).
    pub(crate) fn telemetry_close_run(
        &mut self,
        outcome: kanbei_scheduler::TerminalOutcome,
        usage: kanbei_scheduler::RunUsage,
    ) {
        let Some(span) = self.open_run_span.take() else {
            return;
        };
        let (code, message) = match outcome {
            kanbei_scheduler::TerminalOutcome::Failed(_) => {
                (StatusCode::Error, format!("{outcome:?}"))
            }
            _ => (StatusCode::Ok, format!("{outcome:?}")),
        };
        span.status(code, &message)
            .attr("outcome", AttrValue::Str(format!("{outcome:?}")))
            .attr("tokens", AttrValue::Int(usage.tokens as i64))
            .attr("tools", AttrValue::Int(usage.tools as i64))
            .attr("children", AttrValue::Int(usage.children as i64))
            .end(now_nanos())
            .emit();
    }

    /// Emit the child "commit" span: parent = the open run span when one
    /// exists (else root), attrs = the receipt's canonical seq/counts.
    pub(crate) fn telemetry_commit(&self, receipt: &CommitReceipt) {
        let Some(t) = &self.telemetry else {
            return;
        };
        let now = now_nanos();
        let mut span = t
            .span("commit")
            .kind(SpanKind::Internal)
            .trace_id(self.session_id)
            .span_id(Id128::generate())
            .start(now)
            .attr("first_seq", AttrValue::Int(receipt.first_seq as i64))
            .attr("last_seq", AttrValue::Int(receipt.last_seq as i64))
            .attr("count", AttrValue::Int(receipt.count as i64))
            .attr("frame_len", AttrValue::Int(receipt.frame_len as i64))
            .attr("objects", AttrValue::Int(receipt.objects.len() as i64));
        if let Some(run) = &self.open_run_span {
            span = span.parent(run.span_id_bytes());
        }
        span.end(now).emit();
    }

    /// Emit the "checkpoint" span: the checkpoint's own seq + the pinned
    /// lifetime memory root (both canonical payload values).
    pub(crate) fn telemetry_checkpoint(&self, seq: u64, memory_root: Option<Digest>) {
        let Some(t) = &self.telemetry else {
            return;
        };
        let now = now_nanos();
        let mut span = t
            .span("checkpoint")
            .kind(SpanKind::Internal)
            .trace_id(self.session_id)
            .span_id(Id128::generate())
            .start(now)
            .attr("seq", AttrValue::Int(seq as i64));
        if let Some(root) = memory_root {
            span = span.attr("memory_root", AttrValue::Str(root.to_string()));
        }
        span.end(now).emit();
    }

    /// Emit the "continue_from" span: the transition seq + the new branch
    /// (both canonical record values).
    pub(crate) fn telemetry_continue_from(&self, transition_seq: u64, branch: &BranchId) {
        let Some(t) = &self.telemetry else {
            return;
        };
        let now = now_nanos();
        t.span("continue_from")
            .kind(SpanKind::Internal)
            .trace_id(self.session_id)
            .span_id(Id128::generate())
            .start(now)
            .attr("transition_seq", AttrValue::Int(transition_seq as i64))
            .attr("branch", AttrValue::Str(branch.to_string()))
            .end(now)
            .emit();
    }

    /// Emit the "gc" metric: the canonical swept/restored counts of the
    /// latest GC run (M8 wave 2). No-op without telemetry.
    pub(crate) fn telemetry_gc(&self, report: &kanbei_gc::GcReport) {
        let Some(t) = &self.telemetry else {
            return;
        };
        let session_id = self.session_id.to_string();
        t.metric(
            "kanbei.gc.swept",
            report.swept as i64,
            &[("session_id", AttrValue::Str(session_id.clone()))],
        );
        t.metric(
            "kanbei.gc.restored_or_cleaned",
            report.restored_or_cleaned as i64,
            &[("session_id", AttrValue::Str(session_id))],
        );
    }

    /// Emit the storage gauges (filesystem observations + canonical seq):
    /// objects count/bytes, log bytes/seq, projection bytes.
    pub(crate) fn telemetry_storage(&self) -> Result<(), SessionError> {        let Some(t) = &self.telemetry else {
            return Ok(());
        };
        let session_id = self.session_id.to_string();
        t.metric(
            "kanbei.objects.count",
            self.store().scan()?.len() as i64,
            &[("session_id", AttrValue::Str(session_id.clone()))],
        );
        let mut objects_bytes: i64 = 0;
        for entry in std::fs::read_dir(self.cfg.dir.join("objects"))? {
            objects_bytes += entry?.metadata()?.len() as i64;
        }
        t.metric(
            "kanbei.objects.bytes",
            objects_bytes,
            &[("session_id", AttrValue::Str(session_id.clone()))],
        );
        t.metric(
            "kanbei.log.bytes",
            std::fs::metadata(self.log_path())?.len() as i64,
            &[("session_id", AttrValue::Str(session_id.clone()))],
        );
        // The canonical committed frontier (next_seq = last committed + 1).
        t.metric(
            "kanbei.log.seq",
            self.next_seq.saturating_sub(1) as i64,
            &[("session_id", AttrValue::Str(session_id.clone()))],
        );
        let memory_root = self
            .cfg
            .memory_root
            .clone()
            .unwrap_or_else(|| self.cfg.dir.join("memory"));
        match std::fs::metadata(memory_root.join("projection.sqlite")) {
            Ok(md) => t.metric(
                "kanbei.projection.bytes",
                md.len() as i64,
                &[("session_id", AttrValue::Str(session_id))],
            ),
            // No projection index (memory substrate never built one) — the
            // metric is absent, not zero.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Batch-export every buffered payload through the sink.
    pub(crate) fn telemetry_flush(&self) -> Result<(), SessionError> {
        if let Some(t) = &self.telemetry {
            t.flush()?;
        }
        Ok(())
    }

    /// Emit the storage gauges and flush the exporter (M8 wave 1). No-op
    /// when no telemetry is attached; explicit io errors propagate.
    pub fn report_storage(&self) -> Result<(), SessionError> {
        self.telemetry_storage()?;
        self.telemetry_flush()
    }
}
