//! The kernel's dependency graph is an invariant: invariant enforcement must
//! never depend on a tier-2 service (R-19, decision 14). This is the fast
//! direct-manifest guard; `cargo xtask check-kernel-boundary` additionally
//! walks the resolved graph so transitive leaks cannot hide.

/// The only `kanbei-*` crates the kernel may depend on (tier-1 storage
/// primitives, plus the crate itself in its `[package]` name).
const ALLOWED: &[&str] = &[
    "kanbei-core",
    "kanbei-kernel",
    "kanbei-log",
    "kanbei-objects",
    "kanbei-snapshot",
];

#[test]
fn kernel_declares_only_tier1_kanbei_deps() {
    let manifest = include_str!("../Cargo.toml");
    let mut found: Vec<&str> = Vec::new();
    let mut rest = manifest;
    while let Some(idx) = rest.find("kanbei-") {
        let tail = &rest[idx..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(tail.len());
        found.push(&tail[..end]);
        rest = &tail[end..];
    }
    let violations: Vec<&str> = found
        .into_iter()
        .filter(|name| !ALLOWED.contains(name))
        .collect();
    assert!(
        violations.is_empty(),
        "kanbei-kernel must not declare tier-2 kanbei deps: {violations:?}"
    );
}
