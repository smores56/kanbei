//! The memory error type: every variant carries enough context to debug.

use kanbei_core::Digest;
use thiserror::Error;

use crate::types::IdempotencyKey;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Log(#[from] kanbei_log::RecoveryError),
    #[error(transparent)]
    Object(#[from] kanbei_objects::ObjectError),
    #[error("memory data corrupt: {context}")]
    Corrupt { context: String },
    #[error("missing object: {0}")]
    MissingObject(Digest),
    #[error("duplicate transition for idempotency key {0}")]
    DuplicateTransition(IdempotencyKey),
    #[error("invalid origin: {0}")]
    InvalidOrigin(String),
    #[error("acyclicity violation: {0}")]
    AcyclicViolation(String),
    #[error("root mismatch: expected {expected}, actual {actual}")]
    RootMismatch { expected: Digest, actual: Digest },
    #[error("invalid input: {0}")]
    InvalidInput(String),
}
