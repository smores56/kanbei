//! Integration tests for kanbei-vm against the built kanbei-guest wasm.
//!
//! A missing guest is a hard failure: build it with `cargo xtask build-guest`
//! from the workspace root first.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanbei_vm::{GuestError, Host, Vm, VmConfig};

/// "No epoch limit": the vm caps the config's epoch delta at `u64::MAX / 2`
/// (set_epoch_deadline computes `current + delta` with a plain add, so
/// `u64::MAX` would be a deadline in the past). Non-epoch tests use this so a
/// watchdog bump can never flake them.
const NO_EPOCH: u64 = u64::MAX;

/// Test host: op 0 doubles a number payload (the `kb_host_double` path), op 7
/// echoes the payload into a result object.
struct TestHost;

impl Host for TestHost {
    fn call(&self, _generation_token: u64, op: u32, payload: &str) -> Result<String, String> {
        match op {
            0 => payload
                .parse::<i64>()
                .map(|x| (x * 2).to_string())
                .map_err(|e| format!("op 0: bad payload {payload:?}: {e}")),
            7 => Ok(format!("{{\"op\":{op},\"echo\":{payload}}}")),
            _ => Err(format!("unknown op {op}")),
        }
    }
}

/// Host whose every call reports a stale generation token.
struct StaleHost;

impl Host for StaleHost {
    fn call(&self, _generation_token: u64, _op: u32, _payload: &str) -> Result<String, String> {
        Err("stale generation".into())
    }
}

fn load_vm(config: VmConfig) -> Vm {
    match Vm::load(config) {
        Ok(vm) => vm,
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

/// S1-style config source: 200 key/value entries.
fn config_source() -> String {
    let mut s = String::from("local cfg = {\n");
    for i in 0..200 {
        s.push_str(&format!("  key{i} = \"value-{i}-of-a-configuration-entry\",\n"));
    }
    s.push_str("}\nreturn cfg\n");
    s
}

const BUSY: &str = "local x = 0 for i = 1, 1000000000 do x = x + i end";

#[test]
fn load_and_digest_is_stable() {
    let vm = load_vm(VmConfig::default());
    let d1 = vm.engine_digest();
    let vm2 = Vm::load(VmConfig::default()).expect("second load");
    assert_eq!(d1, vm2.engine_digest());
    assert_eq!(d1.hex().len(), 64);
    assert!(d1.hex().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn compile_ok_and_syntax_error() {
    let vm = load_vm(VmConfig::default());
    let compiled = vm.compile(&config_source()).expect("config-style source compiles");
    drop(compiled);
    let err = vm.compile("local x = = 1").expect_err("syntax error must fail");
    assert!(
        matches!(err, GuestError::Compile(_)),
        "expected Compile, got {err:?}"
    );
}

#[test]
fn instantiate_and_run_script_host_double() {
    let vm = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() });
    let compiled = vm.compile("function kb_hot(x) return x end").expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 7, Arc::new(TestHost))
        .expect("instantiate");
    assert_eq!(inst.generation_token(), 7);
    inst.run_script("assert(kb_host_double(21) == 42)")
        .expect("script calling kb_host_double");
}

#[test]
fn hot_call_json_roundtrip() {
    let vm = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() });
    let compiled = vm
        .compile("function kb_hot(x) return x * 2 end")
        .expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(TestHost))
        .expect("instantiate");
    assert_eq!(inst.call_json("kb_hot", "5").expect("call"), "10");
    assert_eq!(inst.call_json("kb_hot", "-3").expect("call"), "-6");
    // float result serializes with the shortest round-trip form
    let compiled = vm
        .compile("function kb_hot(x) return x / 2 end")
        .expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(TestHost))
        .expect("instantiate");
    assert_eq!(inst.call_json("kb_hot", "5").expect("call"), "2.5");
    // table marshalling: arrays for contiguous integer keys, sorted object keys
    let compiled = vm
        .compile("function kb_hot(x) return { double = x * 2, list = { 1, 2 } } end")
        .expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(TestHost))
        .expect("instantiate");
    assert_eq!(
        inst.call_json("kb_hot", "5").expect("call"),
        r#"{"double":10,"list":[1,2]}"#
    );
    // unknown entry is rejected before touching the guest
    let err = inst.call_json("kb_other", "5").expect_err("unknown entry");
    assert!(matches!(err, GuestError::Host(_)), "got {err:?}");
}

#[test]
fn fuel_trip_and_respawn() {
    let config = VmConfig {
        fuel_per_call: 1_000_000,
        epoch_deadline: NO_EPOCH,
        ..Default::default()
    };
    let vm = load_vm(config);
    let busy = vm
        .compile(&format!("function kb_hot(x) {BUSY} return x end"))
        .expect("compile");
    let mut inst = vm
        .instantiate(&busy, 1, Arc::new(TestHost))
        .expect("instantiate with small fuel budget");
    let err = inst.call_json("kb_hot", "0").expect_err("busy loop trips fuel");
    assert!(
        matches!(err, GuestError::Fuel { .. }),
        "expected Fuel, got {err:?}"
    );
    drop(inst);
    // respawn pattern: a fresh instance on the same vm still works
    let trivial = vm.compile("function kb_hot(x) return x * 2 end").expect("compile");
    let mut inst2 = vm
        .instantiate(&trivial, 1, Arc::new(TestHost))
        .expect("re-instantiate");
    assert_eq!(inst2.call_json("kb_hot", "5").expect("call"), "10");
}

#[test]
fn epoch_trip_with_watchdog() {
    // Unlimited fuel so the epoch bump is the only interruption mechanism.
    let config = VmConfig {
        fuel_per_call: u64::MAX,
        ..Default::default()
    };
    let vm = load_vm(config);
    let busy = vm
        .compile(&format!("function kb_hot(x) {BUSY} return x end"))
        .expect("compile");
    let mut inst = vm
        .instantiate(&busy, 1, Arc::new(TestHost))
        .expect("instantiate");
    let t0 = Instant::now();
    let err = inst.call_json("kb_hot", "0").expect_err("busy loop trips epoch");
    assert!(matches!(err, GuestError::Epoch), "expected Epoch, got {err:?}");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "epoch trip took too long"
    );
}

#[test]
fn memory_limit_trip() {
    // Guest initial memory is 34 pages (~2.1 MB, 1 MB scratch + runtime), so
    // 3 MB lets instantiation succeed while the alloc loop trips growth. If
    // the guest grows past the limit, instantiation itself fails — both paths
    // must surface OutOfMemory.
    let config = VmConfig {
        // Unlimited fuel so the memory limit is the only interruption
        // mechanism (1M fuel trips before the table grows past 3 MB).
        max_memory_bytes: 3 * 1024 * 1024,
        fuel_per_call: u64::MAX,
        epoch_deadline: NO_EPOCH,
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) local t = {} for i = 1, 10000000 do t[i] = i end return t end")
        .expect("compile");
    let mut inst = match vm.instantiate(&compiled, 1, Arc::new(TestHost)) {
        Err(GuestError::OutOfMemory) => {
            eprintln!("note: guest initial memory exceeds the configured limit — instantiation rejected");
            return;
        }
        Ok(i) => i,
        Err(e) => panic!("unexpected instantiate error: {e:?}"),
    };
    let err = inst.call_json("kb_hot", "0").expect_err("alloc loop trips the limit");
    assert!(
        matches!(err, GuestError::OutOfMemory),
        "expected OutOfMemory, got {err:?}"
    );
}

#[test]
fn stale_generation_maps_to_guest_error() {
    let vm = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() });
    let compiled = vm.compile("function kb_hot(x) return x end").expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 99, Arc::new(StaleHost))
        .expect("instantiate");
    let err = inst
        .run_script("local r = kb_host_call(1, '{}') assert(r == 'ok')")
        .expect_err("stale host call must fail");
    assert!(
        matches!(err, GuestError::StaleGeneration),
        "expected StaleGeneration, got {err:?}"
    );
}

#[test]
fn host_call_payload_roundtrip() {
    let vm = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() });
    let compiled = vm.compile("function kb_hot(x) return x end").expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(TestHost))
        .expect("instantiate");
    inst.run_script(
        "local r = kb_host_call(7, '{\"a\":1}') assert(r == '{\"op\":7,\"echo\":{\"a\":1}}')",
    )
    .expect("script with host payload roundtrip");
}

#[test]
fn trap_containment_fresh_instance_still_works() {
    let vm = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() });
    let compiled = vm.compile("function kb_hot(x) return x end").expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(StaleHost))
        .expect("instantiate");
    assert!(matches!(
        inst.run_script("local r = kb_host_call(1, '{}')"),
        Err(GuestError::StaleGeneration)
    ));
    drop(inst);
    // the vm + engine are unaffected: a fresh instance on the same vm works
    let trivial = vm.compile("function kb_hot(x) return x * 2 end").expect("compile");
    let mut inst2 = vm
        .instantiate(&trivial, 1, Arc::new(TestHost))
        .expect("fresh instantiate after trap");
    assert_eq!(inst2.call_json("kb_hot", "5").expect("call"), "10");
    inst2
        .run_script("assert(kb_host_double(21) == 42)")
        .expect("host call after trap");
}

/// Host that blocks forever on every call (for the B-F2 timeout wrapper) and
/// counts retirement calls.
struct HangHost {
    retires: Arc<AtomicUsize>,
}

impl HangHost {
    fn new() -> (Arc<Self>, Arc<AtomicUsize>) {
        let retires = Arc::new(AtomicUsize::new(0));
        (Arc::new(Self { retires: Arc::clone(&retires) }), retires)
    }
}

impl Host for HangHost {
    fn call(&self, _generation_token: u64, _op: u32, _payload: &str) -> Result<String, String> {
        // Hold the sender so `recv` never disconnects: block the worker.
        let (_tx, rx) = mpsc::channel::<()>();
        let _ = rx.recv();
        Ok("never".into())
    }

    fn retire(&self, _generation_token: u64, _reason: &str) {
        self.retires.fetch_add(1, Ordering::Relaxed);
    }
}

/// Host that returns quickly after a short delay (a slow-by-not-hung import),
/// used to confirm the limiter recovers a permit when a worker completes.
struct SlowHost;

impl Host for SlowHost {
    fn call(&self, _generation_token: u64, _op: u32, _payload: &str) -> Result<String, String> {
        std::thread::sleep(Duration::from_millis(20));
        Ok("ok".into())
    }
}

/// Host that counts retirement calls without varying its dispatch behavior.
struct CountHost {
    retires: Arc<AtomicUsize>,
}

impl Host for CountHost {
    fn call(&self, _generation_token: u64, _op: u32, payload: &str) -> Result<String, String> {
        Ok(payload.to_string())
    }

    fn retire(&self, _generation_token: u64, _reason: &str) {
        self.retires.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn host_import_hang_times_out() {
    let config = VmConfig {
        epoch_deadline: NO_EPOCH,
        call_timeout: Duration::from_millis(200),
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) return kb_host_call(7, '{}') end")
        .expect("compile");
    let (host, retires) = HangHost::new();
    let mut inst = vm.instantiate(&compiled, 1, host).expect("instantiate");
    let t0 = Instant::now();
    let err = inst
        .call_json("kb_hot", "0")
        .expect_err("a hanging host import must time out");
    assert!(
        matches!(err, GuestError::HostTimeout { .. }),
        "expected HostTimeout, got {err:?}"
    );
    assert_eq!(
        retires.load(Ordering::Relaxed),
        1,
        "a timed-out import must retire the generation"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "timeout took too long: {:?}",
        t0.elapsed()
    );
}

#[test]
fn host_import_capacity_bounds_abandoned_workers() {
    let config = VmConfig {
        epoch_deadline: NO_EPOCH,
        call_timeout: Duration::from_millis(100),
        max_inflight_host_calls: 2,
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) return kb_host_call(7, '{}') end")
        .expect("compile");
    // Two hangs leak both permits; the third import must fail closed at the
    // ceiling instead of spawning another worker.
    for i in 0..2 {
        let (host, _) = HangHost::new();
        let mut inst = vm.instantiate(&compiled, 1, host).expect("instantiate");
        let err = inst.call_json("kb_hot", "0").expect_err("hang times out");
        assert!(
            matches!(err, GuestError::HostTimeout { .. }),
            "call {i}: {err:?}"
        );
    }
    let (host, _) = HangHost::new();
    let mut inst = vm.instantiate(&compiled, 1, host).expect("instantiate");
    let err = inst.call_json("kb_hot", "0").expect_err("capacity exhausted");
    let GuestError::Host(msg) = err else {
        panic!("expected Host (capacity), got {err:?}");
    };
    assert!(msg.contains("capacity exhausted"), "msg: {msg}");
}

/// T21: per-generation admission is wired per instance, and a generation that
/// wedges a worker cannot starve a fresh generation.
///
/// NOTE: the per-generation ceiling is currently unreachable through a real
/// instance — an actor thread blocks inside a host import, so a generation holds
/// at most one permit, and a wedged import retires the instance (`call_json`
/// marks it dead on `HostTimeout`, lib.rs:922). It is retained deliberately as a
/// defensive bound for a future re-entrant/multi-threaded guest path (T9 hook
/// seams), and its *policy* is pinned by the `host_admission_*` unit tests.
#[test]
fn host_import_per_generation_admission_is_isolated() {
    let config = VmConfig {
        epoch_deadline: NO_EPOCH,
        call_timeout: Duration::from_millis(200),
        max_inflight_host_calls: 32,
        max_inflight_host_calls_per_generation: 1,
        max_abandoned_host_calls: 32,
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) return kb_host_call(7, '{}') end")
        .expect("compile");
    // Generation 1 wedges its single slot, then the vm retires the instance.
    let (hang, _) = HangHost::new();
    let mut inst_a = vm.instantiate(&compiled, 1, hang).expect("instantiate");
    let err = inst_a.call_json("kb_hot", "0").expect_err("hang times out");
    assert!(matches!(err, GuestError::HostTimeout { .. }), "{err:?}");
    let err = inst_a.call_json("kb_hot", "0").expect_err("a retired instance");
    assert!(
        matches!(err, GuestError::Retired { .. }),
        "the instance is retired, not re-admitted: {err:?}"
    );
    // A different generation is admitted regardless of a's wedged worker.
    let mut inst_b = vm
        .instantiate(&compiled, 2, Arc::new(SlowHost))
        .expect("instantiate");
    inst_b
        .call_json("kb_hot", "0")
        .expect("a fresh generation must not be starved by a saturated one");
}

#[test]
fn host_import_capacity_recovers_after_a_slow_call() {
    let config = VmConfig {
        epoch_deadline: NO_EPOCH,
        call_timeout: Duration::from_secs(2),
        max_inflight_host_calls: 1,
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) return kb_host_call(7, '{}') end")
        .expect("compile");
    let mut inst = vm
        .instantiate(&compiled, 1, Arc::new(SlowHost))
        .expect("instantiate");
    inst.call_json("kb_hot", "0")
        .expect("first call completes and releases its permit");
    inst.call_json("kb_hot", "0")
        .expect("second call must not see capacity exhausted");
}

#[test]
fn generation_budget_zero_retires_at_activation() {
    let config = VmConfig {
        epoch_deadline: NO_EPOCH,
        generation_budget: Duration::ZERO,
        ..Default::default()
    };
    let vm = load_vm(config);
    let compiled = vm
        .compile("function kb_hot(x) return x * 2 end")
        .expect("compile");
    let retires = Arc::new(AtomicUsize::new(0));
    let host = Arc::new(CountHost { retires: Arc::clone(&retires) });
    let mut inst = vm.instantiate(&compiled, 1, host).expect("instantiate");
    assert!(inst.is_dead(), "a zero budget is exhausted by activation");
    assert!(
        retires.load(Ordering::Relaxed) >= 1,
        "budget exhaustion must retire the generation"
    );
    let err = inst
        .call_json("kb_hot", "5")
        .expect_err("a retired generation rejects calls");
    assert!(
        matches!(err, GuestError::Retired { .. }),
        "expected Retired, got {err:?}"
    );
}

#[test]
fn generation_budget_accrues_on_trap_path() {
    let config = VmConfig {
        fuel_per_call: 1_000_000,
        epoch_deadline: NO_EPOCH,
        generation_budget: Duration::from_secs(3600),
        ..Default::default()
    };
    let vm = load_vm(config);
    let busy = vm
        .compile(&format!("function kb_hot(x) {BUSY} return x end"))
        .expect("compile");
    let mut inst = vm
        .instantiate(&busy, 1, Arc::new(TestHost))
        .expect("instantiate");
    let before = inst.spent();
    let err = inst
        .call_json("kb_hot", "0")
        .expect_err("busy loop trips fuel");
    assert!(matches!(err, GuestError::Fuel { .. }), "got {err:?}");
    assert!(
        inst.spent() > before,
        "budget must accrue on the trap path (before {before:?}, after {:?})",
        inst.spent()
    );
}

#[test]
fn guest_is_built() {
    Vm::load(VmConfig::default())
        .expect("guest wasm not built: run `cargo xtask build-guest` from the workspace root");
}
