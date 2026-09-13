//! Build automation for the workspace.
//!
//! The guest wasm is a hard prerequisite for the module test battery
//! (decision 19): `Vm::load` returns `GuestError::NotBuilt` when no artifact is
//! embedded, and guest-dependent tests fail rather than skip. `build-guest`
//! compiles it into the path `kanbei-vm/build.rs` probes.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The only target the guest builds for.
pub const GUEST_TARGET: &str = "wasm32-wasip1";
/// Artifact name produced by the guest crate.
pub const GUEST_ARTIFACT: &str = "kanbei_guest.wasm";

/// Where a workspace-root guest build places the artifact. Mirrors the second
/// candidate in `crates/kanbei-vm/build.rs`.
pub fn guest_artifact_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join(GUEST_TARGET)
        .join("release")
        .join(GUEST_ARTIFACT)
}

/// Build the guest wasm for the workspace at `workspace_root`, returning the
/// artifact path. Errors if the build fails or the artifact is not produced.
pub fn build_guest(workspace_root: &Path) -> Result<PathBuf, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(workspace_root)
        .args([
            "build",
            "-p",
            "kanbei-guest",
            "--target",
            GUEST_TARGET,
            "--release",
            "--locked",
        ])
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        return Err(format!("guest build failed: {status}"));
    }
    let artifact = guest_artifact_path(workspace_root);
    if !artifact.is_file() {
        return Err(format!(
            "guest build succeeded but {} is missing",
            artifact.display()
        ));
    }
    // Force kanbei-vm to re-embed: cargo does not rerun its build script when a
    // previously-absent watched path is created, so a vm built against the
    // empty stub would otherwise keep returning GuestError::NotBuilt.
    let status = Command::new(&cargo)
        .current_dir(workspace_root)
        .args(["clean", "-p", "kanbei-vm"])
        .status()
        .map_err(|e| format!("failed to spawn cargo clean: {e}"))?;
    if !status.success() {
        return Err(format!("cargo clean -p kanbei-vm failed: {status}"));
    }
    Ok(artifact)
}

/// The only `kanbei-*` crates the kernel may depend on, directly or
/// transitively.
pub const ALLOWED_KERNEL_DEPS: &[&str] = &[
    "kanbei-core",
    "kanbei-kernel",
    "kanbei-log",
    "kanbei-objects",
    "kanbei-snapshot",
];

/// Walk the resolved dependency graph and fail if `kanbei-kernel` depends
/// (transitively) on any tier-2 crate. This is the enforceable form of the
/// R-19 boundary: tier-2 (and higher) crates are everything `kanbei-*` not in
/// [`ALLOWED_KERNEL_DEPS`], so new crates fail closed.
pub fn check_kernel_boundary(workspace_root: &Path) -> Result<String, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(&cargo)
        .current_dir(workspace_root)
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| format!("failed to spawn cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("bad metadata JSON: {e}"))?;

    let packages = meta["packages"]
        .as_array()
        .ok_or_else(|| "metadata has no packages".to_string())?;
    let name_by_id: HashMap<&str, &str> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p["name"].as_str()?)))
        .collect();
    let kernel_id = packages
        .iter()
        .find(|p| p["name"].as_str() == Some("kanbei-kernel"))
        .and_then(|p| p["id"].as_str())
        .ok_or_else(|| "kanbei-kernel not found in metadata".to_string())?;

    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .ok_or_else(|| "metadata has no resolve graph".to_string())?;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([kernel_id]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = nodes.iter().find(|n| n["id"].as_str() == Some(id))
            && let Some(deps) = node["deps"].as_array()
        {
            for dep in deps {
                if let Some(pkg) = dep["pkg"].as_str() {
                    queue.push_back(pkg);
                }
            }
        }
    }
    let violations: BTreeSet<&str> = seen
        .iter()
        .filter_map(|id| name_by_id.get(id).copied())
        .filter(|name| name.starts_with("kanbei-") && !ALLOWED_KERNEL_DEPS.contains(name))
        .collect();
    if violations.is_empty() {
        Ok(format!(
            "kanbei-kernel depends on {} packages, all tier-1",
            seen.len()
        ))
    } else {
        Err(format!(
            "kanbei-kernel transitively depends on tier-2 crates: {violations:?}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_path_matches_build_rs_probe() {
        let path = guest_artifact_path(Path::new("/ws"));
        assert_eq!(
            path,
            Path::new("/ws/target/wasm32-wasip1/release/kanbei_guest.wasm")
        );
    }

    #[test]
    fn guest_target_is_wasip1() {
        assert_eq!(GUEST_TARGET, "wasm32-wasip1");
    }
}
