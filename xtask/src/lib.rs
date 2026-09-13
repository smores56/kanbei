//! Build automation for the workspace.
//!
//! The guest wasm is a hard prerequisite for the module test battery
//! (decision 19): `Vm::load` returns `GuestError::NotBuilt` when no artifact is
//! embedded, and guest-dependent tests fail rather than skip. `build-guest`
//! compiles it into the path `kanbei-vm/build.rs` probes.

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
