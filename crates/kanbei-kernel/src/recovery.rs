//! Log recovery: a missing log is a valid fresh genesis; any other open error
//! is surfaced.

use std::io;
use std::path::{Path, PathBuf};

use kanbei_log::{Recovered, RecoveryError as LogRecoveryError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("log path is not a file: {0}")]
    NotAFile(PathBuf),
    #[error(transparent)]
    Log(#[from] LogRecoveryError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Recover the log at `log_path`, or report a fresh genesis when it is absent.
pub fn recover_or_fresh(log_path: &Path) -> Result<Recovered, RecoveryError> {
    match std::fs::metadata(log_path) {
        Ok(m) if m.is_file() => Ok(kanbei_log::recover(log_path)?),
        Ok(_) => Err(RecoveryError::NotAFile(log_path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Recovered {
            events: 0,
            frames: 0,
            truncated: false,
            last_seq: 0,
        }),
        Err(e) => Err(RecoveryError::Io(e)),
    }
}
