//! Embeds the compiled kanbei-guest wasm into kanbei-vm.
//!
//! Resolution order: `KANBEI_GUEST_WASM` env var, then the guest's own target
//! dir (standalone guest builds), then the workspace target dir (the guest is
//! a workspace member, so `cargo build -p kanbei-guest --target wasm32-wasip1
//! --release` from the workspace root places the artifact there). If no wasm
//! is found the crate still compiles with an empty stub; `Vm::load` returns
//! `GuestError::NotBuilt` for the stub.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("..").join("..");
    let candidates: Vec<PathBuf> = match env::var_os("KANBEI_GUEST_WASM") {
        // The env var overrides the defaults outright.
        Some(p) => vec![PathBuf::from(p)],
        None => vec![
            workspace_root.join("crates").join("kanbei-guest").join("target").join("wasm32-wasip1").join("release").join("kanbei_guest.wasm"),
            workspace_root.join("target").join("wasm32-wasip1").join("release").join("kanbei_guest.wasm"),
        ],
    };

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("kanbei_guest.wasm");
    match candidates.into_iter().find(|p| p.is_file()) {
        Some(path) => {
            fs::copy(&path, &out).expect("copy guest wasm into OUT_DIR");
            println!("cargo:rerun-if-changed={}", path.display());
        }
        None => {
            println!(
                "cargo:warning=kanbei-guest wasm not found (set KANBEI_GUEST_WASM or run \
                 `cargo build -p kanbei-guest --target wasm32-wasip1 --release`); \
                 embedding empty stub — Vm::load returns NotBuilt"
            );
            fs::write(&out, []).expect("write empty guest stub");
        }
    }
    println!("cargo:rerun-if-env-changed=KANBEI_GUEST_WASM");
}
