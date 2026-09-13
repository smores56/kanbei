//! Manifest pinning: content-addressed install of an execution manifest.

use kanbei_core::digest::Digest;
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_snapshot::ExecutionManifest;

/// Pin `manifest` as an object, returning its digest. Content addressing
/// deduplicates: an unchanged manifest maps to the same digest and is not
/// rewritten.
pub fn pin(store: &mut ObjectStore, manifest: &ExecutionManifest) -> Result<Digest, ObjectError> {
    kanbei_snapshot::pin(store, manifest).map(|(digest, _deduped)| digest)
}
