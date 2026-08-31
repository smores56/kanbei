//! M2 host runtime: sync-only wasmtime 48 execution of the kanbei-guest wasm
//! module (Luaur in wasm).
//!
//! Containment model (docs/architecture.md, one-process containment): a
//! generation runs in its own instance backed by a fresh `Store` with
//! configured limits; interruption is fuel + an epoch-deadline watchdog thread
//! (bumping `Engine::increment_epoch` every `watchdog_tick`); a host-side
//! wall-clock `call_timeout` is checked after every call. Fuel and epoch are
//! the interruption mechanisms — a synchronous wasmtime call cannot be
//! cancelled from another thread — so `call_timeout` is the post-return bound,
//! not an interrupt.
//!
//! After any trap the instance must be dropped and re-instantiated (the S1
//! respawn pattern); the `Vm` (engine + module) is unaffected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kanbei_core::digest::Digest;
use wasmtime::{
    Caller, Config, Engine, Error as WasmError, Extern, Linker, Memory, Module,
    ResourceLimiter, Store, Trap, TypedFunc,
};
use wasmtime_wasi::p1::{self as wasi_p1, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

/// Embedded guest wasm (build.rs copies the real artifact or an empty stub).
const GUEST_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kanbei_guest.wasm"));

const SCRATCH_SIZE: usize = 1 << 20;
/// Bytecode output offset inside the scratch for `kb_compile_out` (source at
/// 0, bytecode past it — the S17 spike pattern).
const BYTECODE_OFFSET: usize = 4096;

/// Guest return codes, see the table in kanbei-guest/src/lib.rs.
const RC_COMPILE: i32 = -2;

/// The exact host-error message the kernel's `Host` impl must return for a
/// stale generation token.
const STALE_GENERATION: &str = "stale generation";

/// Epoch-deadline delta for "no epoch limit". `set_epoch_deadline` computes
/// `current + delta` with a plain add, so `u64::MAX` would wrap to a past
/// deadline (and panic in debug builds); `MAX / 2` is ~2.9M years at the
/// default 10 ms tick.
const NO_EPOCH_LIMIT: u64 = u64::MAX / 2;

fn cap_epoch(delta: u64) -> u64 {
    delta.min(NO_EPOCH_LIMIT)
}

/// Configuration for a `Vm` and the instances it spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// Per-instance linear-memory ceiling in bytes.
    pub max_memory_bytes: usize,
    /// Per-instance table count ceiling.
    pub max_tables: u32,
    /// Per-instance table element ceiling (B-F14): bounds a table without a
    /// module-declared maximum (wasmtime's StoreLimits default is 10_000).
    pub max_table_elements: usize,
    /// Per-instance instance count ceiling.
    pub max_instances: u32,
    /// Fuel budget each guest call (and `kb_init`) starts with.
    pub fuel_per_call: u64,
    /// Epoch-deadline delta per call: the call is interrupted once the
    /// watchdog has ticked this many times during it. Values are capped at
    /// `u64::MAX / 2` (effectively no epoch limit).
    pub epoch_deadline: u64,
    /// Host-side wall-clock bound checked after every call/instantiate.
    pub call_timeout: Duration,
    /// Watchdog epoch-bump period.
    pub watchdog_tick: Duration,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            max_tables: 100,
            max_table_elements: 10_000,
            max_instances: 10,
            fuel_per_call: 1_000_000,
            epoch_deadline: 1,
            call_timeout: Duration::from_secs(5),
            watchdog_tick: Duration::from_millis(10),
        }
    }
}

/// Errors surfaced by the vm.
#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    #[error("guest compile failed: {0}")]
    Compile(String),
    #[error("guest module load failed: {0}")]
    Load(String),
    #[error("wasm trap: {0:?}")]
    Trap(TrapKind),
    #[error("fuel exhausted (consumed {consumed})")]
    Fuel { consumed: u64 },
    #[error("epoch deadline reached")]
    Epoch,
    #[error("out of memory")]
    OutOfMemory,
    #[error("call exceeded host timeout ({elapsed:?})")]
    Timeout { elapsed: Duration },
    #[error("generation token is stale")]
    StaleGeneration,
    #[error("guest returned error code {code}")]
    GuestReturn { code: i32 },
    #[error("host call failed: {0}")]
    Host(String),
    #[error("guest wasm not built (see build.rs)")]
    NotBuilt,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Kinds of wasm trap (see `TrapKind::Other` for anything else).
#[derive(Debug)]
pub enum TrapKind {
    Interrupt,
    Fuel,
    Oom,
    Other(String),
}

/// The kernel-side host effect dispatcher (implemented by kanbei-modules). The
/// impl is responsible for the generation-token currency check: returning
/// `Err("stale generation")` maps to [`GuestError::StaleGeneration`].
pub trait Host: Send + Sync {
    fn call(&self, generation_token: u64, op: u32, payload: &str) -> Result<String, String>;
}

/// Trap markers that cross the wasm boundary as host errors.
#[derive(Debug, thiserror::Error)]
#[error("generation token is stale")]
struct StaleGeneration;

#[derive(Debug, thiserror::Error)]
#[error("host call failed: {0}")]
struct HostFailure(String);

#[derive(Debug, thiserror::Error)]
#[error("memory limit exceeded")]
struct MemoryLimit;

/// Store context: sync resource limiter + wasi (the guest needs no host
/// filesystem).
struct Ctx {
    limiter: Limiter,
    wasi: WasiP1Ctx,
}

/// Per-instance resource limiter. StoreLimits' rejection is graceful at the
/// wasm level (memory.grow returns -1, the guest sees a Lua memory error), so
/// this limiter errors instead — a trap carrying the `MemoryLimit` marker that
/// maps to [`GuestError::OutOfMemory`] (the sync limiter path).
struct Limiter {
    memory_size: usize,
    instances: usize,
    tables: usize,
    /// Per-instance table element ceiling (B-F14): wasmtime's StoreLimits also
    /// caps table elements per store, which bounds a table without a
    /// module-declared maximum.
    max_table_elements: usize,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, WasmError> {
        if desired > self.memory_size {
            return Err(WasmError::new(MemoryLimit));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, WasmError> {
        // B-F14: reject growth past the store's element ceiling too, so an
        // undeclared-maximum table cannot grow without bound. Rejection is
        // graceful here (Ok(false) — the guest's table.grow fails), matching
        // how StoreLimits bounds tables.
        Ok(desired <= self.max_table_elements && maximum.is_none_or(|max| desired <= max))
    }

    fn instances(&self) -> usize {
        self.instances
    }

    fn tables(&self) -> usize {
        self.tables
    }

    fn memories(&self) -> usize {
        1
    }
}

/// Host used on compile-only throwaway instances (host calls never happen
/// during a bare compile).
struct NullHost;

impl Host for NullHost {
    fn call(&self, _generation_token: u64, op: u32, _payload: &str) -> Result<String, String> {
        Err(format!("kb_host op {op}: no host on a compile-only instance"))
    }
}

/// Watchdog thread: bumps the engine epoch every `tick`, bounding runaway
/// guests via each store's epoch deadline.
struct Watchdog {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn spawn(engine: Engine, tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(tick);
                engine.increment_epoch();
            }
        });
        Self { stop, handle: Some(handle) }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The host runtime: engine + compiled guest module + watchdog.
pub struct Vm {
    engine: Engine,
    module: Module,
    config: VmConfig,
    digest: Digest,
    _watchdog: Watchdog,
}

/// The opaque compiled artifact: deterministic Luau bytecode (S17) plus the
/// source, which the instance needs because the guest's hot cache is populated
/// via `kb_init(source)` (no bytecode-load export in M2).
#[derive(Debug, Clone)]
pub struct CompiledModule {
    source: String,
    // Deterministic bytecode artifact (S17); consumed by M3 execution-snapshot
    // digests once the guest gains a bytecode-load export.
    #[allow(dead_code)]
    bytecode: Vec<u8>,
}

/// A live guest instance (one generation). After any trap it must be dropped
/// and re-instantiated.
pub struct Instance {
    store: Store<Ctx>,
    scratch: TypedFunc<(), i32>,
    hot_call_str: TypedFunc<(i32, i32, i32, i32), i32>,
    run: TypedFunc<(i32, i32), i32>,
    memory: Memory,
    fuel_per_call: u64,
    epoch_deadline: u64,
    call_timeout: Duration,
    generation_token: u64,
}

struct GuestExports {
    scratch: TypedFunc<(), i32>,
    compile_out: TypedFunc<(i32, i32, i32, i32), i32>,
    init: TypedFunc<(i32, i32), i32>,
    hot_call_str: TypedFunc<(i32, i32, i32, i32), i32>,
    run: TypedFunc<(i32, i32), i32>,
    memory: Memory,
}

fn api_error(what: &str, e: impl std::fmt::Display) -> GuestError {
    GuestError::Host(format!("wasmtime ({what}): {e}"))
}

fn memory_error(what: &str, e: impl std::fmt::Display) -> GuestError {
    GuestError::Host(format!("guest memory ({what}): {e}"))
}

/// Map a call-time wasmtime error to a `GuestError`, using the host markers
/// first and falling back to trap codes. `fuel_consumed` is only meaningful
/// for `Trap::OutOfFuel`.
fn trap_error(e: WasmError, fuel_consumed: u64) -> GuestError {
    if e.downcast_ref::<StaleGeneration>().is_some() {
        return GuestError::StaleGeneration;
    }
    if let Some(h) = e.downcast_ref::<HostFailure>() {
        return GuestError::Host(h.0.clone());
    }
    if e.downcast_ref::<MemoryLimit>().is_some() || e.downcast_ref::<wasmtime::OutOfMemory>().is_some()
    {
        return GuestError::OutOfMemory;
    }
    if let Some(t) = e.downcast_ref::<Trap>() {
        return match t {
            Trap::Interrupt => GuestError::Epoch,
            Trap::OutOfFuel => GuestError::Fuel { consumed: fuel_consumed },
            other => GuestError::Trap(TrapKind::Other(other.to_string())),
        };
    }
    GuestError::Trap(TrapKind::Other(e.to_string()))
}

fn guest_code(code: i32) -> GuestError {
    match code {
        RC_COMPILE => GuestError::Compile("guest compile failed".into()),
        other => GuestError::GuestReturn { code: other },
    }
}

/// Compiled-guest cache keyed by the wasm digest + wasmtime version: codegen
/// (wasmtime 48 has no public `deserialize`-based cache config) is paid once
/// per machine, then the serialized artifact is reloaded.
/// Trust boundary: the cache file is local user state produced by this same
/// binary; `Module::deserialize` is unsafe and we accept that trust.
fn load_or_compile_guest(engine: &Engine) -> Result<Module, GuestError> {
    let wasm_digest = Digest::new(GUEST_WASM);
    let key = format!("guest-{}-wasmtime-48.cwasm", wasm_digest.hex());
    let dir = std::env::var("KANBEI_VM_CACHE_DIR").unwrap_or_else(|_| {
        let base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cache")))
            .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
        format!("{base}/kanbei")
    });
    let path = std::path::Path::new(&dir).join(&key);
    if let Ok(bytes) = std::fs::read(&path) {
        // SAFETY: bytes came from our own serialize() in this cache file.
        if let Ok(m) = unsafe { Module::deserialize(engine, &bytes) } {
            return Ok(m);
        }
    }
    let module = Module::new(engine, GUEST_WASM).map_err(|e| GuestError::Load(e.to_string()))?;
    if let Ok(bytes) = module.serialize() {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    Ok(module)
}

impl Vm {
    /// Load the embedded guest wasm and spawn the watchdog.
    pub fn load(config: VmConfig) -> Result<Self, GuestError> {
        if GUEST_WASM.is_empty() {
            return Err(GuestError::NotBuilt);
        }
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        // Single-threaded codegen: wasmtime's default parallel compilation
        // fans out a rayon pool (all cores) per Module compile, which melts
        // CPU when many generations/Vm instances are spun up in sequence.
        cfg.parallel_compilation(false);
        let engine = Engine::new(&cfg).map_err(|e| GuestError::Load(e.to_string()))?;
        let module = load_or_compile_guest(&engine)?;
        let digest = Digest::new(GUEST_WASM);
        let watchdog = Watchdog::spawn(engine.clone(), config.watchdog_tick);
        Ok(Self { engine, module, config, digest, _watchdog: watchdog })
    }

    /// blake3 digest of the embedded guest wasm bytes (execution-snapshot
    /// manifest `engine_digest`, R-08/E-12).
    pub fn engine_digest(&self) -> Digest {
        self.digest
    }

    /// Compile `source` to deterministic Luau bytecode in a fresh throwaway
    /// instance (unlimited fuel; the epoch deadline is off during compile).
    pub fn compile(&self, source: &str) -> Result<CompiledModule, GuestError> {
        let t0 = Instant::now();
        let (mut store, exports) = self.instantiate_guest(Arc::new(NullHost), 0, u64::MAX, NO_EPOCH_LIMIT)?;
        let base = exports
            .scratch
            .call(&mut store, ())
            .map_err(|e| trap_error(e, 0))? as usize;
        exports
            .memory
            .write(&mut store, base, source.as_bytes())
            .map_err(|e| memory_error("compile source", e))?;
        let out = base + BYTECODE_OFFSET;
        let cap = SCRATCH_SIZE - BYTECODE_OFFSET;
        let n = exports
            .compile_out
            .call(&mut store, (base as i32, source.len() as i32, out as i32, cap as i32))
            .map_err(|e| trap_error(e, 0))?;
        let elapsed = t0.elapsed();
        if elapsed > self.config.call_timeout {
            return Err(GuestError::Timeout { elapsed });
        }
        if n < 0 {
            return Err(guest_code(n));
        }
        let mut bytecode = vec![0u8; n as usize];
        exports
            .memory
            .read(&mut store, out, &mut bytecode)
            .map_err(|e| memory_error("compile bytecode", e))?;
        Ok(CompiledModule { source: source.to_string(), bytecode })
    }

    /// Instantiate a fresh store for `compiled` (running `kb_init` so the
    /// guest caches `kb_hot`), with configured limits, fuel, and host
    /// dispatchers wrapping `host.call(token, ..)`.
    pub fn instantiate(
        &self,
        compiled: &CompiledModule,
        generation_token: u64,
        host: Arc<dyn Host>,
    ) -> Result<Instance, GuestError> {
        let t0 = Instant::now();
        let (mut store, exports) =
            self.instantiate_guest(host, generation_token, self.config.fuel_per_call, NO_EPOCH_LIMIT)?;
        let base = exports
            .scratch
            .call(&mut store, ())
            .map_err(|e| trap_error(e, self.config.fuel_per_call))? as usize;
        exports
            .memory
            .write(&mut store, base, compiled.source.as_bytes())
            .map_err(|e| memory_error("kb_init source", e))?;
        let code = exports
            .init
            .call(&mut store, (base as i32, compiled.source.len() as i32))
            .map_err(|e| {
                let remaining = store.get_fuel().unwrap_or(0);
                trap_error(e, self.config.fuel_per_call.saturating_sub(remaining))
            })?;
        let elapsed = t0.elapsed();
        if elapsed > self.config.call_timeout {
            return Err(GuestError::Timeout { elapsed });
        }
        if code < 0 {
            return Err(guest_code(code));
        }
        Ok(Instance {
            store,
            scratch: exports.scratch,
            hot_call_str: exports.hot_call_str,
            run: exports.run,
            memory: exports.memory,
            fuel_per_call: self.config.fuel_per_call,
            epoch_deadline: cap_epoch(self.config.epoch_deadline),
            call_timeout: self.config.call_timeout,
            generation_token,
        })
    }

    /// Fresh store + linker with the host dispatchers, then instantiate the
    /// guest module and grab its exports.
    fn instantiate_guest(
        &self,
        host: Arc<dyn Host>,
        generation_token: u64,
        fuel: u64,
        epoch_delta: u64,
    ) -> Result<(Store<Ctx>, GuestExports), GuestError> {
        let limiter = Limiter {
            memory_size: self.config.max_memory_bytes,
            instances: self.config.max_instances as usize,
            tables: self.config.max_tables as usize,
            max_table_elements: self.config.max_table_elements,
        };
        let wasi = WasiCtxBuilder::new().build_p1();
        let mut store = Store::new(&self.engine, Ctx { limiter, wasi });
        store.set_fuel(fuel).map_err(|e| api_error("set_fuel", e))?;
        store.set_epoch_deadline(epoch_delta);
        store.limiter(|c| &mut c.limiter);

        let mut linker = Linker::new(&self.engine);
        wasi_p1::add_to_linker_sync(&mut linker, |c: &mut Ctx| &mut c.wasi)
            .map_err(|e| api_error("wasi linker", e))?;
        link_host_dispatchers(&mut linker, host, generation_token)?;

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| trap_error(e, 0))?;
        let exports = GuestExports {
            scratch: instance
                .get_typed_func(&mut store, "kb_scratch")
                .map_err(|e| api_error("kb_scratch", e))?,
            compile_out: instance
                .get_typed_func(&mut store, "kb_compile_out")
                .map_err(|e| api_error("kb_compile_out", e))?,
            init: instance
                .get_typed_func(&mut store, "kb_init")
                .map_err(|e| api_error("kb_init", e))?,
            hot_call_str: instance
                .get_typed_func(&mut store, "kb_hot_call_str")
                .map_err(|e| api_error("kb_hot_call_str", e))?,
            run: instance
                .get_typed_func(&mut store, "kb_run")
                .map_err(|e| api_error("kb_run", e))?,
            memory: instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| GuestError::Load("guest exports no \"memory\"".into()))?,
        };
        Ok((store, exports))
    }
}

/// Link the two dispatcher imports. Both wrap `host.call(token, ..)`; a host
/// `Err("stale generation")` traps with the `StaleGeneration` marker, any
/// other `Err` traps with `HostFailure`.
fn link_host_dispatchers(
    linker: &mut Linker<Ctx>,
    host: Arc<dyn Host>,
    generation_token: u64,
) -> Result<(), GuestError> {
    let host_buf = Arc::clone(&host);
    linker
        .func_wrap("env", "kb_host", move |op: i32, x: i32| -> Result<i32, WasmError> {
            let payload = x.to_string();
            match host.call(generation_token, op as u32, &payload) {
                Ok(s) => s.parse::<i32>().map_err(|e| {
                    WasmError::new(HostFailure(format!(
                        "kb_host op {op}: result {s:?} is not an i32: {e}"
                    )))
                }),
                Err(msg) if msg == STALE_GENERATION => Err(WasmError::new(StaleGeneration)),
                Err(msg) => Err(WasmError::new(HostFailure(format!("kb_host op {op}: {msg}")))),
            }
        })
        .map_err(|e| api_error("kb_host", e))?;

    linker
        .func_wrap(
            "env",
            "kb_host_buf",
            move |mut caller: Caller<'_, Ctx>, op: i32, ptr: i32, len: i32| -> Result<i32, WasmError> {
                let memory = match caller.get_export("memory") {
                    Some(Extern::Memory(m)) => m,
                    _ => {
                        return Err(WasmError::new(HostFailure(
                            "kb_host_buf: guest exports no memory".into(),
                        )));
                    }
                };
                let (ptr, len) = (ptr as usize, len as usize);
                if ptr.saturating_add(len) > memory.data_size(&caller) {
                    return Err(WasmError::new(HostFailure(format!(
                        "kb_host_buf: payload out of bounds ({ptr}+{len})"
                    ))));
                }
                let mut payload = vec![0u8; len];
                memory
                    .read(&caller, ptr, &mut payload)
                    .map_err(|e| WasmError::new(HostFailure(format!("kb_host_buf: read: {e}"))))?;
                let payload = String::from_utf8(payload)
                    .map_err(|_| WasmError::new(HostFailure("kb_host_buf: payload is not UTF-8".into())))?;
                let result = match host_buf.call(generation_token, op as u32, &payload) {
                    Ok(s) => s,
                    Err(msg) if msg == STALE_GENERATION => return Err(WasmError::new(StaleGeneration)),
                    Err(msg) => {
                        return Err(WasmError::new(HostFailure(format!(
                            "kb_host_buf op {op}: {msg}"
                        ))));
                    }
                };
                let result = result.into_bytes();
                if ptr.saturating_add(result.len()) > memory.data_size(&caller) {
                    return Err(WasmError::new(HostFailure(format!(
                        "kb_host_buf op {op}: result too large"
                    ))));
                }
                memory
                    .write(&mut caller, ptr, &result)
                    .map_err(|e| WasmError::new(HostFailure(format!("kb_host_buf op {op}: write: {e}"))))?;
                Ok(result.len() as i32)
            },
        )
        .map_err(|e| api_error("kb_host_buf", e))?;
    Ok(())
}

impl Instance {
    /// Call the cached `kb_hot` with one JSON value (`args`); returns the
    /// JSON-serialized result. Marshalling shape: args JSON → single Lua
    /// value → `kb_hot` → single result value → JSON (see the guest's
    /// `kb_hot_call_str`). Each call resets fuel and the epoch deadline, so a
    /// call gets a fresh `epoch_deadline`-tick window.
    pub fn call_json(&mut self, entry: &str, args: &str) -> Result<String, GuestError> {
        if entry != "kb_hot" {
            return Err(GuestError::Host(format!(
                "unknown call entry {entry:?}: only \"kb_hot\" is cached (kb_init)"
            )));
        }
        let t0 = Instant::now();
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| api_error("set_fuel", e))?;
        self.store.set_epoch_deadline(self.epoch_deadline);
        let base = self
            .scratch
            .call(&mut self.store, ())
            .map_err(|e| trap_error(e, self.fuel_per_call))? as usize;
        self.memory
            .write(&mut self.store, base, args.as_bytes())
            .map_err(|e| memory_error("call_json args", e))?;
        let res = self.hot_call_str.call(
            &mut self.store,
            (base as i32, args.len() as i32, base as i32, SCRATCH_SIZE as i32),
        );
        let n = match res {
            Ok(n) => n,
            Err(e) => {
                let remaining = self.store.get_fuel().unwrap_or(0);
                return Err(trap_error(e, self.fuel_per_call.saturating_sub(remaining)));
            }
        };
        let elapsed = t0.elapsed();
        if elapsed > self.call_timeout {
            return Err(GuestError::Timeout { elapsed });
        }
        if n < 0 {
            return Err(guest_code(n));
        }
        let mut buf = vec![0u8; n as usize];
        self.memory
            .read(&mut self.store, base, &mut buf)
            .map_err(|e| memory_error("call_json result", e))?;
        String::from_utf8(buf).map_err(|_| GuestError::Host("guest returned non-UTF-8 JSON".into()))
    }

    /// The generation token this instance was instantiated with.
    pub fn generation_token(&self) -> u64 {
        self.generation_token
    }

    /// Compile + run `source` with the host functions available (for tests).
    pub fn run_script(&mut self, source: &str) -> Result<(), GuestError> {
        let t0 = Instant::now();
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| api_error("set_fuel", e))?;
        self.store.set_epoch_deadline(self.epoch_deadline);
        let base = self
            .scratch
            .call(&mut self.store, ())
            .map_err(|e| trap_error(e, self.fuel_per_call))? as usize;
        self.memory
            .write(&mut self.store, base, source.as_bytes())
            .map_err(|e| memory_error("run_script source", e))?;
        let res = self.run.call(&mut self.store, (base as i32, source.len() as i32));
        let code = match res {
            Ok(code) => code,
            Err(e) => {
                let remaining = self.store.get_fuel().unwrap_or(0);
                return Err(trap_error(e, self.fuel_per_call.saturating_sub(remaining)));
            }
        };
        let elapsed = t0.elapsed();
        if elapsed > self.call_timeout {
            return Err(GuestError::Timeout { elapsed });
        }
        match code {
            0 => Ok(()),
            other => Err(guest_code(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B-F14: a table without a module-declared maximum is still bounded by
    /// the store's element ceiling, and the rejection is graceful (the guest
    /// sees a failed table.grow).
    #[test]
    fn table_growth_bounded_without_declared_maximum() {
        let mut l = Limiter {
            memory_size: 1024,
            instances: 1,
            tables: 1,
            max_table_elements: 10,
        };
        assert!(l.table_growing(0, 10, None).unwrap(), "growth to the ceiling passes");
        assert!(
            !l.table_growing(10, 11, None).unwrap(),
            "growth past the ceiling is rejected without a declared maximum"
        );
        // The module-declared maximum still applies on its own.
        assert!(!l.table_growing(0, 11, Some(10)).unwrap());
        assert!(l.table_growing(0, 10, Some(10)).unwrap());
        assert!(l.table_growing(0, 10, Some(50)).unwrap());
    }
}
