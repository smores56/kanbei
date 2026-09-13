//! Writer pins: digests with an install in flight (or an external writer's
//! in-flight reference) that GC must never quarantine or sweep.

use std::collections::HashSet;
use std::sync::Mutex;

use kanbei_core::digest::Digest;

/// Registers every digest a commit installs with the writer-pin set and
/// unregisters them on drop — including every error return path — so a failed
/// commit never leaks pins.
pub struct GcPinGuard<'a> {
    pins: &'a Mutex<HashSet<Digest>>,
    added: Vec<Digest>,
}

impl<'a> GcPinGuard<'a> {
    pub fn new(pins: &'a Mutex<HashSet<Digest>>) -> Self {
        Self {
            pins,
            added: Vec::new(),
        }
    }

    /// Registers `digest` before its install (idempotent).
    pub fn pin(&mut self, digest: Digest) {
        self.pins
            .lock()
            .expect("gc pins lock poisoned")
            .insert(digest);
        self.added.push(digest);
    }
}

impl Drop for GcPinGuard<'_> {
    fn drop(&mut self) {
        let mut pins = self.pins.lock().expect("gc pins lock poisoned");
        for digest in &self.added {
            pins.remove(digest);
        }
    }
}
