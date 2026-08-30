//! The retrieval error type: every variant carries enough context to debug.

use std::io;

use kanbei_core::Digest;
use kanbei_memory::MemoryError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RetrievalError {
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("missing object: {0}")]
    MissingObject(Digest),
    #[error(transparent)]
    Memory(#[from] MemoryError),
}
