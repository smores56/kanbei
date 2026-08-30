//! M9 wave 4: content-addressed working-tree snapshots and restore — the
//! session entry points over kanbei-workspace. Each call commits a canonical
//! event (`workspace_snapshot` / `workspace_restore`) referencing the
//! manifest digest, so snapshots are durable, recoverable, and GC-reachable
//! like any other committed object (no GC changes needed — manifest and
//! blobs are ordinary objects).

use kanbei_core::digest::Digest;
use serde_json::json;

use crate::{NewEvent, Session, SessionError};

impl Session {
    /// Snapshots `self.fs_root` into the session object store and commits a
    /// canonical `workspace_snapshot` event (`{"root", "manifest", "entries"}`
    /// payload; the manifest digest as the event's only ref). Returns the
    /// manifest digest — the root digest of the snapshot.
    pub fn snapshot_workspace(
        &mut self,
        options: kanbei_workspace::SnapshotOptions,
    ) -> Result<Digest, SessionError> {
        let manifest = kanbei_workspace::snapshot(&mut self.store, &self.fs_root, &options)?;
        let parsed: kanbei_workspace::Manifest = serde_json::from_slice(
            &self
                .store
                .get(&manifest)
                .map_err(SessionError::Object)?,
        )
        .map_err(|e| {
            SessionError::InvalidInput(format!("installed workspace manifest parse: {e}"))
        })?;
        self.commit(
            vec![NewEvent {
                kind: "workspace_snapshot".into(),
                payload_schema: 1,
                payload: json!({
                    "root": self.fs_root.to_string_lossy(),
                    "manifest": manifest.to_string(),
                    "entries": parsed.entries.len(),
                }),
                objects: Vec::new(),
                refs: vec![manifest],
            }],
            Some(self.composition.current().digest),
        )?;
        Ok(manifest)
    }

    /// Restores the tree pinned by `manifest` into `self.fs_root` (additive/
    /// overwrite, never a wipe — see kanbei-workspace) and commits a
    /// canonical `workspace_restore` event with the manifest digest and the
    /// restored counts. On restore failure no event is committed — the error
    /// propagates untouched.
    pub fn restore_workspace(
        &mut self,
        manifest: &Digest,
    ) -> Result<kanbei_workspace::RestoreReport, SessionError> {
        let report = kanbei_workspace::restore(&self.store, manifest, &self.fs_root)?;
        self.commit(
            vec![NewEvent {
                kind: "workspace_restore".into(),
                payload_schema: 1,
                payload: json!({
                    "manifest": manifest.to_string(),
                    "entries_restored": report.entries_restored,
                    "bytes": report.bytes,
                    "symlinks": report.symlinks,
                }),
                objects: Vec::new(),
                refs: vec![*manifest],
            }],
            Some(self.composition.current().digest),
        )?;
        Ok(report)
    }
}
