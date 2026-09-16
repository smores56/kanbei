//! kanbei-modules — the M2 unified module lifecycle (architecture.md
//! "Unified module lifecycle" and "Module state"; R-07, R-24/C-04, R-25/C-05,
//! R-25/C-06): immutable packages, module/generation identities,
//! activation/disposal, generation replacement with stale-effect rejection,
//! host-owned module state (the head CAS contract), and the kernel
//! [`kanbei_vm::Host`] impl routing vm host calls.
//!
//! Files: [`package`] (immutable packages), [`state`] (host-owned module
//! state), [`lifecycle`] (the `ModuleManager`), [`runtime`] (the per-generation
//! actor that owns each Wasmtime store), [`host`] (the kernel host ABI). The M2
//! guest ABI is the Luau contract in `lifecycle::ACTIVATION_SHIM` plus the op
//! table documented on `host::ModuleHost`.
//!
//! `ModuleError` embeds `kanbei_services::ServiceError` whose variants carry
//! unboxed `ServiceProvider`/`ServiceKey` values (a fixed public contract,
//! mirroring kanbei-services' own `#![allow(clippy::result_large_err)]`).
#![allow(clippy::result_large_err)]

pub mod host;
pub mod lifecycle;
pub mod package;
mod runtime;
pub mod state;

pub use host::ModuleHost;
pub use lifecycle::{
    DisposalRecord, Generation, HookError, ModuleError, ModuleManager, ReplacementOutcome,
    HOOK_WAIT,
};
pub use package::{
    install_package, ModuleOrigin, PackageError, PackageManifest, PackageStore, PACKAGE_SCHEMA,
};
pub use runtime::{ActorError, GenerationRuntime};
pub use state::{HeadFile, StateError, StateStore, StateUpdate};
