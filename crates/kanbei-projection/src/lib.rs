//! kanbei-projection — the disposable SQLite projection (rebuild) and the M1
//! audit reconstruction report (R-06/S6 shape, docs/spikes/s6-report.md).
//! Rebuild streams the canonical log (S5: `for_each_frame` chain+digest
//! verification), reconstructs the per-kind audit report (S6), and loads a
//! fresh SQLite projection. R-23: watermarks commit in the same transaction
//! as the rows they cover; rebuild ignores existing watermarks (the schema
//! is dropped and recreated — rebuild is authoritative).
//! Docs: docs/architecture.md, docs/spikes/ratification-packet.md.

use std::io;
use std::path::Path;

use kanbei_core::envelope::EnvelopeError;
use kanbei_core::registry::{Registry, Report};
use kanbei_core::Envelope;
use kanbei_log::{for_each_frame, RecoveryError};
use kanbei_objects::{ObjectError, ObjectStore};
use rusqlite::{params, Connection, Statement};
use serde_json::Value;
use thiserror::Error;

/// Schema version of the `events` table this crate creates.
pub const PROJECTION_SCHEMA: u32 = 1;

/// Events per SQLite transaction during rebuild (S5's tx/1000).
pub const TX_BATCH: usize = 1000;

/// Drop + recreate fresh: rebuild is authoritative and ignores any
/// pre-existing projection state or watermarks (R-23).
const SCHEMA_SQL: &str = "
DROP TABLE IF EXISTS events;
DROP TABLE IF EXISTS watermarks;
CREATE TABLE IF NOT EXISTS events (
  seq INTEGER PRIMARY KEY,
  evt TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload_schema INTEGER NOT NULL,
  payload TEXT NOT NULL,       -- canonical JSON (verbatim)
  refs TEXT NOT NULL,          -- JSON array of digest strings
  snapshot TEXT                -- pre-event snapshot digest or NULL
);
CREATE TABLE IF NOT EXISTS watermarks (stream TEXT PRIMARY KEY, last_seq INTEGER NOT NULL);
";

const INSERT_EVENT_SQL: &str =
    "INSERT INTO events (seq, evt, kind, payload_schema, payload, refs, snapshot) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

#[derive(Debug, Error)]
pub enum RebuildError {
    #[error(transparent)]
    Log(#[from] RecoveryError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("envelope: {0}")]
    Envelope(EnvelopeError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    InvalidInput(String),
}

/// The M1 audit reconstruction contract (S6 shape, no SQLite): stream the
/// chain-verified log and account every event per kind — schema/count/
/// upcasted/opaque with the first opaque reason — plus deduped
/// insertion-ordered missing object references and upcaster error strings.
/// Unknown kinds stay opaque-but-inspectable (R-06); missing required
/// objects are precise partial availability, never fabricated.
pub fn reconstruct(
    log_path: &Path,
    registry: &Registry,
    store: &ObjectStore,
) -> Result<Report, RebuildError> {
    let mut rep = Report::default();
    let mut first_err: Option<RebuildError> = None;
    let rec = for_each_frame(log_path, |frame| {
        if first_err.is_some() {
            return;
        }
        for line in &frame.events {
            match parse_event(line) {
                Ok(env) => account_event(&env, registry, store, &mut rep),
                Err(e) => {
                    first_err = Some(e);
                    return;
                }
            }
        }
    });
    match first_err {
        // an event-level failure earlier in the stream wins over a later
        // frame-level failure
        Some(e) => Err(e),
        None => rec.map(|_| rep).map_err(RebuildError::Log),
    }
}

/// The disposable SQLite projection: [`reconstruct`] plus a fresh WAL
/// `synchronous=OFF` load (disposable, no durability claim) in one streaming
/// pass, one transaction per [`TX_BATCH`] events. The watermark row
/// (last frame's stream) commits in the same transaction as the rows it
/// covers (R-23).
pub fn rebuild(
    log_path: &Path,
    db_path: &Path,
    registry: &Registry,
    store: &ObjectStore,
) -> Result<Report, RebuildError> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.execute_batch(SCHEMA_SQL)?;

    let mut stmt = conn.prepare(INSERT_EVENT_SQL)?;
    conn.execute_batch("BEGIN")?;
    let mut in_tx = 0usize;
    let mut last_stream: Option<String> = None;
    let mut rep = Report::default();
    let mut first_err: Option<RebuildError> = None;

    let rec = for_each_frame(log_path, |frame| {
        if first_err.is_some() {
            return;
        }
        last_stream = Some(frame.meta.stream.clone());
        for line in &frame.events {
            if let Err(e) =
                insert_event(line, &conn, &mut stmt, registry, store, &mut rep, &mut in_tx)
            {
                first_err = Some(e);
                return;
            }
        }
    });

    if let Some(e) = first_err {
        // open transaction rolls back when the connection drops
        return Err(e);
    }
    let rec = rec.map_err(RebuildError::Log)?;
    if let Some(stream) = last_stream {
        conn.execute(
            "INSERT INTO watermarks (stream, last_seq) VALUES (?1, ?2)",
            params![stream, rec.last_seq as i64],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(rep)
}

/// Parse one event line and run the kernel envelope invariant checks. Any
/// failure is `InvalidInput` naming the offending seq (the frame chain and
/// digests were already verified by [`for_each_frame`]).
fn parse_event(line: &str) -> Result<Envelope, RebuildError> {
    let env = Envelope::from_line(line).map_err(|e| {
        let ctx = line_seq(line)
            .map(|seq| format!("event at seq {seq}"))
            .unwrap_or_else(|| "event with unreadable seq".to_string());
        RebuildError::InvalidInput(format!("{ctx}: invalid envelope JSON: {e}"))
    })?;
    env.validate().map_err(|e| {
        RebuildError::InvalidInput(format!("event at seq {}: envelope invalid: {e}", env.seq))
    })?;
    Ok(env)
}

/// Best-effort seq extraction from a line that failed to parse as an envelope.
fn line_seq(line: &str) -> Option<u64> {
    serde_json::from_str::<Value>(line).ok()?.get("seq")?.as_u64()
}

/// Apply one validated envelope to the audit report. Per-kind accounting
/// copied from S6: schema/count/upcasted/opaque with the first opaque reason
/// (`no upcaster for kind '<kind>' schema <schema>` for unknown kinds);
/// upcaster errors count as opaque and are reported with kind+schema context.
/// Missing object references are deduped, insertion order kept.
fn account_event(env: &Envelope, registry: &Registry, store: &ObjectStore, rep: &mut Report) {
    rep.events += 1;
    let stat = rep.kinds.entry(env.kind.clone()).or_default();
    stat.schema = env.payload_schema;
    stat.count += 1;
    match registry.upcast(&env.kind, env.payload_schema, &env.payload) {
        Ok(Some(_)) => stat.upcasted += 1,
        Ok(None) => {
            stat.opaque += 1;
            stat.opaque_reason.get_or_insert_with(|| {
                format!("no upcaster for kind '{}' schema {}", env.kind, env.payload_schema)
            });
        }
        Err(e) => {
            stat.opaque += 1;
            stat.opaque_reason.get_or_insert_with(|| e.clone());
            rep.upcast_errors
                .push(format!("kind '{}' schema {}: {e}", env.kind, env.payload_schema));
        }
    }
    for r in &env.refs {
        if !store.exists(r) {
            let s = r.to_string();
            if !rep.missing_objects.contains(&s) {
                rep.missing_objects.push(s);
            }
        }
    }
}

/// Parse, account, and insert one event; commits the open transaction when a
/// batch fills, before the next insert, so the final batch stays open for the
/// watermark row (R-23).
fn insert_event(
    line: &str,
    conn: &Connection,
    stmt: &mut Statement,
    registry: &Registry,
    store: &ObjectStore,
    rep: &mut Report,
    in_tx: &mut usize,
) -> Result<(), RebuildError> {
    if *in_tx >= TX_BATCH {
        conn.execute_batch("COMMIT; BEGIN")?;
        *in_tx = 0;
    }
    let env = parse_event(line)?;
    account_event(&env, registry, store, rep);
    let payload = verbatim_payload(line)?;
    let refs_json = serde_json::to_string(&env.refs).map_err(|e| {
        RebuildError::InvalidInput(format!("event at seq {}: serialize refs: {e}", env.seq))
    })?;
    stmt.execute(params![
        env.seq as i64,
        env.evt,
        env.kind,
        env.payload_schema as i64,
        payload,
        refs_json,
        env.snapshot.map(|s| s.to_string()),
    ])?;
    *in_tx += 1;
    Ok(())
}

/// Byte-exact payload text from a canonical envelope line. Re-serializing the
/// parsed `Value` would reorder object keys (serde_json maps are sorted),
/// violating R-06 verbatim retention. In the canonical single-line envelope,
/// `"payload":` first occurs at the field key (an unescaped quote can never
/// precede `payload` inside a JSON string), so the value runs from there to
/// its closing brace/bracket (or the next comma) at depth zero.
fn verbatim_payload(line: &str) -> Result<&str, RebuildError> {
    const KEY: &str = "\"payload\":";
    let start = line
        .find(KEY)
        .ok_or_else(|| {
            RebuildError::InvalidInput("event: envelope missing payload field".to_string())
        })?
        + KEY.len();
    let bytes = line.as_bytes();
    let mut i = start;
    let mut depth = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        if depth == 0 && (b == b',' || b == b'}') {
            break;
        }
        match b {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(&line[start..i])
}
