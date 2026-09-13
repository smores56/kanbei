//! Canonical commit input/output types.

use kanbei_core::digest::Digest;

/// One caller-authored event, not yet sequenced or validated.
#[derive(Debug)]
pub struct NewEvent {
    pub kind: String,
    pub payload_schema: u32,
    pub payload: serde_json::Value,
    /// Installed as objects before the frame is appended; their digests are
    /// appended to `refs` (R-10).
    pub objects: Vec<Vec<u8>>,
    /// Must already exist in the store — a commit never creates a dangling
    /// reference.
    pub refs: Vec<Digest>,
}

/// What a committed batch consumed: sequence span, frame size, installed
/// object digests, and the manifest digests bracketing the commit.
#[derive(Debug)]
pub struct CommitReceipt {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
    /// Digests installed by this commit's object phase (event objects +
    /// promoted payloads; the post-state manifest, if any, is excluded).
    pub objects: Vec<Digest>,
    /// The manifest digest every envelope in this commit references (R-08);
    /// None when the session resumed without manifest state (M1).
    pub pre_snapshot: Option<Digest>,
    /// The manifest pinned because this commit changed state; None for pure
    /// commits (unchanged manifests dedup via content addressing).
    pub post_snapshot: Option<Digest>,
}
