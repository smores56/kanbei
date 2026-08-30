//! Optional OpenTelemetry-compatible telemetry (M8 wave 1): correlation
//! spans and storage gauges emitted as OTLP/HTTP JSON documents, handed to
//! a [`Sink`] — no collector dependency, no OTLP client.
//!
//! Seam: payloads are protobuf-JSON wire-compatible with the OTLP/HTTP
//! `ExportTraceServiceRequest` / `ExportMetricsServiceRequest` shapes
//! (`resourceSpans` / `resourceMetrics`); a future HTTP/gRPC exporter
//! implements [`Sink`] by POSTing each payload to the collector's
//! `/v1/traces` / `/v1/metrics` endpoint. [`FileSink`] appends
//! newline-delimited payloads for offline inspection. The emitter is
//! hand-rolled (no OTLP crate, no base64 crate) per the workspace
//! minimal-dependency discipline: `bytes` fields (`traceId`, `spanId`,
//! `parentSpanId`) use standard base64 and `int64` fields
//! (`*TimeUnixNano`, `asInt`, `intValue`) use decimal strings, per the
//! protobuf-JSON encoding.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kanbei_core::id::Id128;

// ---------- sinks ----------

/// The export seam: one OTLP JSON payload per call. Implementations choose
/// framing — [`FileSink`] appends a newline; an HTTP sink would POST.
pub trait Sink: Send + Sync {
    fn export(&self, payload: &[u8]) -> io::Result<()>;
}

/// Appends newline-delimited OTLP JSON payloads to a path (created on
/// first export; no fsync — telemetry is observation, not state).
#[derive(Debug, Clone)]
pub struct FileSink {
    path: PathBuf,
}

impl FileSink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Sink for FileSink {
    fn export(&self, payload: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(payload)?;
        f.write_all(b"\n")
    }
}

// ---------- OTLP value types ----------

/// OTLP `Span.SpanKind` (proto enum values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SpanKind {
    Unspecified = 0,
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

/// OTLP `Status.StatusCode` (proto enum values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum StatusCode {
    Unset = 0,
    Ok = 1,
    Error = 2,
}

/// An OTLP `AnyValue` attribute value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl AttrValue {
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    pub fn int(value: i64) -> Self {
        Self::Int(value)
    }

    pub fn bool(value: bool) -> Self {
        Self::Bool(value)
    }
}

// ---------- the handle ----------

/// Buffers spans and gauges; [`Telemetry::flush`] batch-exports every
/// buffered payload through the sink. Cheap to clone (Arc interior);
/// Send + Sync.
#[derive(Clone)]
pub struct Telemetry {
    pending: Arc<Mutex<Vec<String>>>,
    sink: Arc<dyn Sink>,
}

impl Telemetry {
    pub fn new(sink: Arc<dyn Sink>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(Vec::new())),
            sink,
        }
    }

    /// Start a span builder; callers set trace/span ids and timestamps
    /// explicitly for correlation.
    pub fn span(&self, name: &str) -> SpanBuilder {
        SpanBuilder {
            pending: Arc::clone(&self.pending),
            name: name.to_string(),
            ..SpanBuilder::default()
        }
    }

    /// Queue one gauge (a single `resourceMetrics` payload, exported at
    /// the next flush).
    pub fn metric(&self, name: &str, value: i64, attrs: &[(&str, AttrValue)]) {
        let payload = metric_payload(name, value, attrs, now_nanos());
        self.pending
            .lock()
            .expect("telemetry pending poisoned")
            .push(payload);
    }

    /// Export every buffered payload through the sink. Sink errors are
    /// explicit and abort the batch (remaining payloads are dropped).
    pub fn flush(&self) -> io::Result<()> {
        let pending = std::mem::take(&mut *self.pending.lock().expect("telemetry pending poisoned"));
        for payload in pending {
            self.sink.export(payload.as_bytes())?;
        }
        Ok(())
    }
}

/// A span under construction; [`SpanBuilder::end`] serializes it as one
/// `resourceSpans` payload and queues it for export.
#[derive(Debug)]
pub struct SpanBuilder {
    pending: Arc<Mutex<Vec<String>>>,
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    name: String,
    kind: SpanKind,
    start_nanos: u64,
    end_nanos: u64,
    attributes: Vec<(String, AttrValue)>,
    status: (StatusCode, String),
}

impl Default for SpanBuilder {
    fn default() -> Self {
        Self {
            pending: Arc::new(Mutex::new(Vec::new())),
            trace_id: [0; 16],
            span_id: [0; 8],
            parent_span_id: None,
            name: String::new(),
            kind: SpanKind::Unspecified,
            start_nanos: 0,
            end_nanos: 0,
            attributes: Vec::new(),
            status: (StatusCode::Unset, String::new()),
        }
    }
}

impl SpanBuilder {
    /// The trace id: a canonical kanbei id (Id128 is exactly 16 bytes —
    /// the OTLP trace-id width).
    pub fn trace_id(mut self, id: Id128) -> Self {
        self.trace_id = *id.as_bytes();
        self
    }

    /// The span id: a canonical kanbei id's first 8 bytes (OTLP span ids
    /// are 8 bytes).
    pub fn span_id(mut self, id: Id128) -> Self {
        self.span_id.copy_from_slice(&id.as_bytes()[..8]);
        self
    }

    /// The parent span's id (from [`SpanBuilder::span_id`]).
    pub fn parent(mut self, span_id: [u8; 8]) -> Self {
        self.parent_span_id = Some(span_id);
        self
    }

    pub fn kind(mut self, kind: SpanKind) -> Self {
        self.kind = kind;
        self
    }

    /// Start timestamp (unix nanos).
    pub fn start(mut self, nanos: u64) -> Self {
        self.start_nanos = nanos;
        self
    }

    /// End timestamp (unix nanos).
    pub fn end(mut self, nanos: u64) -> Self {
        self.end_nanos = nanos;
        self
    }

    pub fn attr(mut self, key: &str, value: AttrValue) -> Self {
        self.attributes.push((key.to_string(), value));
        self
    }

    pub fn status(mut self, code: StatusCode, message: &str) -> Self {
        self.status = (code, message.to_string());
        self
    }

    /// The span's id — the parent handle for child spans.
    pub fn span_id_bytes(&self) -> [u8; 8] {
        self.span_id
    }

    /// Serialize the span as one `resourceSpans` payload and queue it for
    /// export (at the next [`Telemetry::flush`]).
    pub fn emit(self) {
        let payload = span_payload(&self);
        self.pending
            .lock()
            .expect("telemetry pending poisoned")
            .push(payload);
    }
}

// ---------- OTLP protobuf-JSON rendering ----------

fn span_payload(span: &SpanBuilder) -> String {
    format!(
        r#"{{"resourceSpans":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"kanbei"}}}}]}},"scopeSpans":[{{"scope":{{"name":"kanbei.session"}},"spans":[{}]}}]}}]}}"#,
        span_json(span)
    )
}

fn span_json(span: &SpanBuilder) -> String {
    let parent = match span.parent_span_id {
        Some(id) => format!(r#""parentSpanId":"{}","#, base64(&id)),
        None => String::new(),
    };
    let mut attrs = String::new();
    if !span.attributes.is_empty() {
        attrs.push_str(r#""attributes":["#);
        for (i, (k, v)) in span.attributes.iter().enumerate() {
            if i > 0 {
                attrs.push(',');
            }
            let _ = write!(attrs, r#"{{"key":{},"value":{}}}"#, json_str(k), any_value(v));
        }
        attrs.push_str("],");
    }
    let (code, message) = &span.status;
    let status = if message.is_empty() {
        format!(r#"{{"code":{}}}"#, *code as u32)
    } else {
        format!(
            r#"{{"code":{},"message":{}}}"#,
            *code as u32,
            json_str(message)
        )
    };
    format!(
        r#"{{"traceId":"{}","spanId":"{}",{}"name":{},"kind":{},"startTimeUnixNano":"{}","endTimeUnixNano":"{}",{}"status":{}}}"#,
        base64(&span.trace_id),
        base64(&span.span_id),
        parent,
        json_str(&span.name),
        span.kind as u32,
        span.start_nanos,
        span.end_nanos,
        attrs,
        status,
    )
}

fn metric_payload(name: &str, value: i64, attrs: &[(&str, AttrValue)], now: u64) -> String {
    let mut entries = String::new();
    for (i, (k, v)) in attrs.iter().enumerate() {
        if i > 0 {
            entries.push(',');
        }
        let _ = write!(entries, r#"{{"key":{},"value":{}}}"#, json_str(k), any_value(v));
    }
    format!(
        r#"{{"resourceMetrics":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"kanbei"}}}}]}},"scopeMetrics":[{{"scope":{{"name":"kanbei.storage"}},"metrics":[{{"name":{},"gauge":{{"dataPoints":[{{"attributes":[{entries}],"asInt":"{value}","timeUnixNano":"{now}"}}]}}}}]}}]}}]}}"#,
        json_str(name)
    )
}

fn any_value(v: &AttrValue) -> String {
    match v {
        AttrValue::Str(s) => format!(r#"{{"stringValue":{}}}"#, json_str(s)),
        // protobuf-JSON encodes int64 as a decimal string
        AttrValue::Int(i) => format!(r#"{{"intValue":"{i}"}}"#),
        AttrValue::Bool(b) => format!(r#"{{"boolValue":{b}}}"#),
    }
}

/// Minimal JSON string escaping (the payload shapes are fixed; only values
/// are caller-controlled).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const B64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (the OTLP JSON encoding of `bytes`).
fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(B64_ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct VecSink(Arc<Mutex<Vec<String>>>);

    impl Sink for VecSink {
        fn export(&self, payload: &[u8]) -> io::Result<()> {
            self.0
                .lock()
                .expect("test sink poisoned")
                .push(String::from_utf8_lossy(payload).into_owned());
            Ok(())
        }
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            base64(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
            "AAECAwQFBgcICQoLDA0ODw=="
        );
    }

    #[test]
    fn span_payload_shape() {
        let sink = VecSink::default();
        let telemetry = Telemetry::new(Arc::new(sink.clone()));
        let trace = Id128::generate();
        let span_id = Id128::generate();
        let parent = Id128::generate();
        let parent_bytes: [u8; 8] = parent.as_bytes()[..8].try_into().unwrap();
        telemetry
            .span("run")
            .kind(SpanKind::Server)
            .trace_id(trace)
            .span_id(span_id)
            .parent(parent_bytes)
            .start(1_700_000_000_000_000_000)
            .attr("session_id", AttrValue::Str("ses_abc".into()))
            .attr("tokens", AttrValue::Int(42))
            .attr("hot", AttrValue::Bool(true))
            .status(StatusCode::Ok, "Progress")
            .end(1_700_000_000_100_000_000)
            .emit();
        telemetry.flush().unwrap();
        let payload = &sink.0.lock().expect("test sink poisoned")[0];
        assert!(payload.contains(r#""resourceSpans":"#));
        assert!(payload.contains(r#""resource":{"attributes":[{"key":"service.name","value":{"stringValue":"kanbei"}}]}"#));
        assert!(payload.contains(r#""scope":{"name":"kanbei.session"}"#));
        assert!(payload.contains(&format!(
            r#""traceId":"{}","spanId":"{}","parentSpanId":"{}","name":"run","kind":2,"startTimeUnixNano":"1700000000000000000","endTimeUnixNano":"1700000000100000000""#,
            base64(trace.as_bytes()),
            base64(&span_id.as_bytes()[..8]),
            base64(&parent.as_bytes()[..8]),
        )));
        assert!(payload.contains(r#""attributes":[{"key":"session_id","value":{"stringValue":"ses_abc"}},{"key":"tokens","value":{"intValue":"42"}},{"key":"hot","value":{"boolValue":true}}]"#));
        assert!(payload.contains(r#""status":{"code":1,"message":"Progress"}"#));
    }

    #[test]
    fn root_span_omits_parent_field() {
        let sink = VecSink::default();
        let telemetry = Telemetry::new(Arc::new(sink.clone()));
        telemetry
            .span("commit")
            .kind(SpanKind::Internal)
            .trace_id(Id128::generate())
            .span_id(Id128::generate())
            .end(1)
            .emit();
        telemetry.flush().unwrap();
        let payload = &sink.0.lock().expect("test sink poisoned")[0];
        assert!(!payload.contains("parentSpanId"));
        assert!(payload.contains(r#""name":"commit","kind":1"#));
        assert!(payload.contains(r#""status":{"code":0}"#));
    }

    #[test]
    fn metric_payload_shape() {
        let sink = VecSink::default();
        let telemetry = Telemetry::new(Arc::new(sink.clone()));
        telemetry.metric(
            "kanbei.objects.count",
            5,
            &[("session_id", AttrValue::Str("ses_abc".into()))],
        );
        telemetry.flush().unwrap();
        let payload = &sink.0.lock().expect("test sink poisoned")[0];
        assert!(payload.contains(r#""resourceMetrics":"#));
        assert!(payload.contains(r#""scope":{"name":"kanbei.storage"}"#));
        assert!(payload.contains(r#""name":"kanbei.objects.count","gauge":{"dataPoints":[{"attributes":[{"key":"session_id","value":{"stringValue":"ses_abc"}}],"asInt":"5","timeUnixNano":""#));
    }

    #[test]
    fn json_str_escapes() {
        assert_eq!(json_str("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        assert_eq!(json_str("\u{7}"), r#""\u0007""#);
    }

    #[test]
    fn file_sink_appends_newline_delimited() {
        let dir = std::env::temp_dir().join(format!(
            "kb-telemetry-test-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let path = dir.join("out.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        let sink = FileSink::new(&path);
        sink.export(br#"{"a":1}"#).unwrap();
        sink.export(br#"{"b":2}"#).unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        assert_eq!(file, "{\"a\":1}\n{\"b\":2}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
