//! kanbei-core — the M1 durable-kernel foundation: branded ids (`id`), content
//! digests (`digest`), the event envelope (`envelope`), the upcaster registry
//! (`registry`), and the durability queue (`queue`). Design inputs:
//! docs/spikes/ratification-packet.md, spikes/s6-upcast.

pub mod digest;
pub mod envelope;
pub mod id;
pub mod queue;
pub mod registry;

pub use digest::{Digest, DigestParseError, ALG};
pub use envelope::{Envelope, EnvelopeError, ENVELOPE_SCHEMA};
pub use id::{
    parse_branded_any, BranchId, BrandedId, BrandedParseError, Id128, Id128ParseError, BRANDS,
    UUID_BYTES,
};
pub use queue::{DurabilityQueue, SyncOp};
pub use registry::{KindStat, Registry, RegistryError, Report, Upcaster};
