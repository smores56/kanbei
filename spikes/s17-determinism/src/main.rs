//! S17 spike: bytecode determinism within a Luaur version (native and
//! in-guest), informing E-12 (engine/toolchain digests in the snapshot
//! manifest). Disposable spike code — never promoted into the implementation.

use std::path::Path;

use wasmtime::{Config, Engine, Linker, Memory, Module, Store, TypedFunc};
use wasmtime_wasi::p1::{self as wasi_p1, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

const SRC: &str = r#"
local cfg = { retries = 3, backoff = 1.5 }
local function plan(trigger)
    if trigger == "idle" then return { action = "reflect", n = cfg.retries } end
    return { action = "act", backoff = cfg.backoff }
end
return plan
"#;

fn sha(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn bench_native() {
    let mut hashes = Vec::new();
    let mut size = 0usize;
    for _ in 0..1000 {
        let bc = luaur::compile(SRC).unwrap();
        size = bc.len();
        hashes.push(sha(&bc));
    }
    let first = &hashes[0];
    let all_same = hashes.iter().all(|h| h == first);
    println!("native: 1000 compiles, all identical = {all_same}, bytecode {size} B, digest {first}");
    assert!(all_same);
}

struct GuestExports {
    compile_out: TypedFunc<(i32, i32, i32, i32), i32>,
    scratch: TypedFunc<(), i32>,
    memory: Memory,
}

/// Compile the source in one fresh instance and return the bytecode bytes.
fn guest_bytecode(engine: &Engine, module: &Module) -> Vec<u8> {
    let mut linker = Linker::new(engine);
    wasi_p1::add_to_linker_sync(&mut linker, |c: &mut WasiP1Ctx| c).unwrap();
    linker.func_wrap("env", "kb_host", |op: i32, x: i32| match op { 0 => x * 2, _ => -1 }).unwrap();
    let wasi = WasiCtxBuilder::new().build_p1();
    let mut store = Store::new(engine, wasi);
    store.set_fuel(u64::MAX).unwrap();
    let instance = linker.instantiate(&mut store, module).unwrap();
    let exports = GuestExports {
        compile_out: instance.get_typed_func(&mut store, "kb_compile_out").unwrap(),
        scratch: instance.get_typed_func(&mut store, "kb_scratch").unwrap(),
        memory: instance.get_memory(&mut store, "memory").expect("memory"),
    };
    let base = exports.scratch.call(&mut store, ()).unwrap() as usize;
    let out = base + 4096; // scratch is 1 MB; source goes at 0, bytecode at 4 KB
    exports.memory.write(&mut store, base, SRC.as_bytes()).unwrap();
    let n = exports.compile_out.call(&mut store, (base as i32, SRC.len() as i32, out as i32, 1 << 20)).unwrap();
    assert!(n > 0, "guest compile failed: {n}");
    let mut bytes = vec![0u8; n as usize];
    exports.memory.read(&mut store, out, &mut bytes).unwrap();
    bytes
}

fn bench_guest(wasm_path: &Path) {
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    let engine = Engine::new(&cfg).unwrap();
    let module = Module::from_file(&engine, wasm_path).unwrap();
    let mut hashes = Vec::new();
    let mut size = 0usize;
    for _ in 0..10 {
        let bc = guest_bytecode(&engine, &module);
        size = bc.len();
        hashes.push(sha(&bc));
    }
    let first = &hashes[0];
    let all_same = hashes.iter().all(|h| h == first);
    println!("guest: 10 fresh instances, bytecode identical = {all_same}, {size} B, digest {first}");
    assert!(all_same);
    // cross-process determinism: the digest printed here must match the next
    // process's run (checked by the caller)
    println!("CROSS_PROCESS_DIGEST {first}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()).unwrap_or("") {
        "native" => bench_native(),
        "guest" => {
            let path = args.get(2).expect("path to kb_guest.wasm");
            bench_guest(Path::new(path));
        }
        _ => {
            eprintln!("usage: kb-s17-determinism <native|guest <kb_guest.wasm>>");
        }
    }
}
