use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under the workspace root")
        .to_path_buf()
}

fn main() {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("build-guest") => match xtask::build_guest(&workspace_root()) {
            Ok(artifact) => println!("guest wasm built: {}", artifact.display()),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some("check-kernel-boundary") => match xtask::check_kernel_boundary(&workspace_root()) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("usage: cargo xtask <build-guest|check-kernel-boundary>");
            std::process::exit(2);
        }
    }
}
