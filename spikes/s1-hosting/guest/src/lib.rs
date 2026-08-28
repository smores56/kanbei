//! S1 spike guest: Luaur embedded in a wasm32-wasip1 cdylib, driven by the host harness.
//! Disposable spike code — never promoted into the implementation.
//!
//! One dispatcher host import (`kb_host`) carries all host calls: the real harness
//! ABI is a small fixed set of host entry points, and multiple same-signature wasm
//! imports are unreliable under rust-lld's wasm GC/link path.

use std::slice;
use std::sync::Mutex;

/// Scratch buffer the host writes inputs into before calling an export.
#[unsafe(no_mangle)]
pub static mut KB_SCRATCH: [u8; 1 << 20] = [0u8; 1 << 20];

/// Host dispatcher: `kb_host(op, x)`. op 0 = sync double, op 1 = async double.
unsafe extern "C" {
    fn kb_host(op: i32, x: i32) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn kb_scratch() -> *mut u8 {
    std::ptr::addr_of_mut!(KB_SCRATCH).cast::<u8>()
}

fn src<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).ok()
}

/// Persistent hot-path VM: holds one compiled generation so host→guest calls
/// don't recompile. Empty until kb_init.
static HOT: Mutex<Option<(luaur::rt::Lua, luaur::rt::Function)>> = Mutex::new(None);

/// Load `src`, keep the VM, and store the global `kb_hot` function for kb_hot_call.
#[unsafe(no_mangle)]
pub extern "C" fn kb_init(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return -1 };
    let lua = luaur::rt::Lua::new();
    if lua.load(s).exec().is_err() {
        return -6;
    }
    let f = match lua.globals().get::<luaur::rt::Function>("kb_hot") {
        Ok(f) => f,
        Err(_) => return -7,
    };
    let mut guard = HOT.lock().unwrap();
    *guard = Some((lua, f));
    0
}

/// Call the cached `kb_hot(x)` function. Returns its result, or a negative code.
#[unsafe(no_mangle)]
pub extern "C" fn kb_hot_call(x: i32) -> i32 {
    let guard = HOT.lock().unwrap();
    let Some((_, f)) = guard.as_ref() else { return -8 };
    match f.call::<i32>(x) {
        Ok(v) => v,
        Err(_) => -9,
    }
}

/// Compile Luau source only (config-compile latency proxy). 0 on success, negative on failure.
#[unsafe(no_mangle)]
pub extern "C" fn kb_compile(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return -1 };
    match luaur::compile(s) {
        Ok(_) => 0,
        Err(_) => -2,
    }
}

/// Compile + run Luau source. Host functions are exposed to the script.
#[unsafe(no_mangle)]
pub extern "C" fn kb_run(ptr: *const u8, len: usize) -> i32 {
    let Some(s) = src(ptr, len) else { return -1 };
    let lua = luaur::rt::Lua::new();
    let sync = lua.create_function(|_, x: i32| Ok(unsafe { kb_host(0, x) }));
    let async_ = lua.create_function(|_, x: i32| Ok(unsafe { kb_host(1, x) }));
    let Ok(sync) = sync else { return -4 };
    let Ok(async_) = async_ else { return -4 };
    if lua.globals().set("kb_host_double", sync).is_err() {
        return -5;
    }
    if lua.globals().set("kb_host_async", async_).is_err() {
        return -5;
    }
    match lua.load(s).exec() {
        Ok(()) => 0,
        Err(_) => -6,
    }
}
