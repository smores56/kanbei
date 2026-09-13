//! Embeds the compiled kanbei-guest wasm into kanbei-vm.
//!
//! Resolution order: `KANBEI_GUEST_WASM` env var, then the workspace target
//! dir (the guest is a workspace member, so `cargo xtask build-guest` places
//! the artifact there), then the guest's own target dir (standalone builds).
//! If no wasm is found the crate still compiles with an empty stub and
//! `Vm::load` returns `GuestError::NotBuilt`; guest-dependent tests treat that
//! as a hard failure (build it with `cargo xtask build-guest`). Existing
//! candidate artifacts are watched, so updating the guest re-embeds it;
//! `cargo xtask build-guest` also cleans kanbei-vm so a first-time build
//! re-embeds without a manual `cargo clean -p kanbei-vm`.

use std::env;
use std::fs;
use std::path::PathBuf;

/// WebAssembly magic: every valid module starts with `\0asm`.
const WASM_MAGIC: &[u8] = b"\0asm";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("..").join("..");
    let workspace_candidate = workspace_root
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join("kanbei_guest.wasm");
    let overridden = env::var_os("KANBEI_GUEST_WASM").is_some();
    let candidates: Vec<PathBuf> = if overridden {
        vec![PathBuf::from(env::var_os("KANBEI_GUEST_WASM").unwrap())]
    } else {
        vec![
            workspace_candidate.clone(),
            workspace_root
                .join("crates")
                .join("kanbei-guest")
                .join("target")
                .join("wasm32-wasip1")
                .join("release")
                .join("kanbei_guest.wasm"),
        ]
    };

    // Watch existing candidates so an updated artifact re-embeds. A candidate
    // that appears after this crate was last built is handled by
    // `cargo xtask build-guest`, which cleans kanbei-vm to force re-embedding
    // (cargo does not rerun a build script when a previously-absent watched
    // path is created).
    for path in &candidates {
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("kanbei_guest.wasm");
    match candidates.into_iter().find(|p| p.is_file()) {
        Some(path) => {
            let bytes = fs::read(&path).expect("read guest wasm");
            assert!(
                bytes.starts_with(WASM_MAGIC),
                "guest artifact {} is not a wasm module (bad magic); unset \
                 KANBEI_GUEST_WASM or run `cargo xtask build-guest`",
                path.display()
            );
            fs::write(&out, bytes).expect("copy guest wasm into OUT_DIR");
        }
        None => {
            println!(
                "cargo:warning=kanbei-guest wasm not found (set KANBEI_GUEST_WASM or run \
                 `cargo xtask build-guest`); embedding empty stub — Vm::load returns \
                 NotBuilt and guest-dependent tests fail."
            );
            fs::write(&out, []).expect("write empty guest stub");
        }
    }
    println!("cargo:rerun-if-env-changed=KANBEI_GUEST_WASM");
}
