//! The event envelope: the kernel-validated record shape stored in AppendLog
//! frames. Shape = S6 (`{env, seq, evt, kind, schema, payload, refs}`) plus
//! the M1 `snapshot` field (pre-event execution-snapshot digest, R-08).
//! `snapshot: None` serializes as JSON null so the field is always present.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::digest::Digest;

pub const ENVELOPE_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Envelope {
    pub env: u32,
    pub seq: u64,
    pub evt: String,
    pub kind: String,
    #[serde(rename = "schema")]
    pub payload_schema: u32,
    pub payload: Value,
    #[serde(default)]
    pub refs: Vec<Digest>,
    #[serde(default)]
    pub snapshot: Option<Digest>,
}

impl Envelope {
    /// Canonical single-line JSON, the on-disk and on-wire form.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap()
    }

    pub fn from_line(line: &str) -> Result<Envelope, serde_json::Error> {
        serde_json::from_str(line)
    }

    /// Kernel-side invariant check, run before an envelope is stored.
    /// `refs`/`snapshot` are typed [`Digest`]s, so those checks verify the
    /// canonical text form round-trips.
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.env != ENVELOPE_SCHEMA {
            return Err(EnvelopeError::BadEnv(self.env));
        }
        if self.seq == 0 {
            return Err(EnvelopeError::SeqZero(self.seq));
        }
        if self.evt.is_empty() {
            return Err(EnvelopeError::EmptyEvt);
        }
        if self.kind.is_empty() {
            return Err(EnvelopeError::EmptyKind);
        }
        if self.payload_schema == 0 {
            return Err(EnvelopeError::BadPayloadSchema(self.payload_schema));
        }
        for r in &self.refs {
            if r.to_string().parse::<Digest>() != Ok(*r) {
                return Err(EnvelopeError::InvalidRef);
            }
        }
        if let Some(s) = self.snapshot
            && s.to_string().parse::<Digest>() != Ok(s)
        {
            return Err(EnvelopeError::InvalidSnapshot);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvelopeError {
    #[error("env {0}, expected {ENVELOPE_SCHEMA}")]
    BadEnv(u32),
    #[error("seq {0} must be >= 1")]
    SeqZero(u64),
    #[error("evt must be non-empty")]
    EmptyEvt,
    #[error("kind must be non-empty")]
    EmptyKind,
    #[error("payload schema {0} must be >= 1")]
    BadPayloadSchema(u32),
    #[error("refs entry is not a valid digest")]
    InvalidRef,
    #[error("snapshot is not a valid digest")]
    InvalidSnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Envelope {
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 1,
            evt: "e1".into(),
            kind: "user_message".into(),
            payload_schema: 1,
            payload: json!({"text": "hi"}),
            refs: vec![Digest::new(b"obj")],
            snapshot: Some(Digest::new(b"snap")),
        }
    }

    #[test]
    fn to_line_roundtrip() {
        let env = valid();
        let line = env.to_line();
        assert!(!line.contains('\n'));
        let back = Envelope::from_line(&line).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn exact_json_shape() {
        let env = Envelope {
            payload: json!(null),
            refs: vec![],
            snapshot: None,
            ..valid()
        };
        // field order as declared; snapshot: None must serialize as null
        assert_eq!(
            env.to_line(),
            r#"{"env":1,"seq":1,"evt":"e1","kind":"user_message","schema":1,"payload":null,"refs":[],"snapshot":null}"#
        );
    }

    #[test]
    fn validate_ok() {
        valid().validate().unwrap();
    }

    #[test]
    fn validate_rejects_bad_env() {
        let env = Envelope { env: 2, ..valid() };
        assert!(matches!(env.validate(), Err(EnvelopeError::BadEnv(2))));
    }

    #[test]
    fn validate_rejects_seq_zero() {
        let env = Envelope { seq: 0, ..valid() };
        assert!(matches!(env.validate(), Err(EnvelopeError::SeqZero(0))));
    }

    #[test]
    fn validate_rejects_empty_kind() {
        let env = Envelope { kind: String::new(), ..valid() };
        assert!(matches!(env.validate(), Err(EnvelopeError::EmptyKind)));
    }

    #[test]
    fn validate_rejects_empty_evt() {
        let env = Envelope { evt: String::new(), ..valid() };
        assert!(matches!(env.validate(), Err(EnvelopeError::EmptyEvt)));
    }

    #[test]
    fn validate_rejects_payload_schema_zero() {
        let env = Envelope {
            payload_schema: 0,
            ..valid()
        };
        assert!(matches!(
            env.validate(),
            Err(EnvelopeError::BadPayloadSchema(0))
        ));
    }
}
