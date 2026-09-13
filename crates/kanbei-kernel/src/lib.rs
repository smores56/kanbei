//! The tier-1 enforcement kernel (R-19): mechanisms and invariants only.
//!
//! The kernel owns the commit path (objects-first install, reference
//! verification, payload classification, one appended frame, post-state
//! manifest pinning), log recovery, writer pins, and the manifest-pinning
//! primitive. Its dependency graph is storage primitives only
//! (`kanbei-core`/`kanbei-log`/`kanbei-objects`/`kanbei-snapshot`); it never
//! depends on a tier-2 service to enforce an invariant. Tier-2 crates depend
//! on the kernel and implement its service traits (for example the
//! [`FaultInjector`] the testkit supplies).

pub mod commit;
pub mod event;
pub mod fault;
pub mod pinning;
pub mod pins;
pub mod recovery;

pub use commit::{
    CommitError, CommitOutcome, CommitParams, CommitPath, PostManifest, pin_post_manifest,
};
pub use event::{CommitReceipt, NewEvent};
pub use fault::{FaultInjector, FaultPoint};
pub use pins::GcPinGuard;
