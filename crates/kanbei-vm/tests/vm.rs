//! Integration tests for kanbei-vm against the built kanbei-guest wasm.
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from the
//! workspace root first; without it every test prints `skip:` and passes.

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

fn load_vm(config: VmConfig) -> Option<Vm> {
    match Vm::load(config) {
        Ok(vm) => Some(vm),
        Err(GuestError::NotBuilt) => {
            eprintln!("skip: guest wasm not built (see build.rs)");
            None
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
    let Some(vm) = load_vm(VmConfig::default()) else { return };
    let d1 = vm.engine_digest();
    let vm2 = Vm::load(VmConfig::default()).expect("second load");
    assert_eq!(d1, vm2.engine_digest());
    assert_eq!(d1.hex().len(), 64);
    assert!(d1.hex().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn compile_ok_and_syntax_error() {
    let Some(vm) = load_vm(VmConfig::default()) else { return };
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
    let Some(vm) = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() }) else {
        return;
    };
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
    let Some(vm) = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() }) else {
        return;
    };
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
    let Some(vm) = load_vm(config) else { return };
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
    let Some(vm) = load_vm(config) else { return };
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
    let Some(vm) = load_vm(config) else { return };
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
    let Some(vm) = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() }) else {
        return;
    };
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
    let Some(vm) = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() }) else {
        return;
    };
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
    let Some(vm) = load_vm(VmConfig { epoch_deadline: NO_EPOCH, ..Default::default() }) else {
        return;
    };
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

#[test]
fn not_built_when_stub_embedded() {
    match Vm::load(VmConfig::default()) {
        Err(GuestError::NotBuilt) => {
            eprintln!("note: guest wasm absent — stub embedded, NotBuilt confirmed");
        }
        Ok(_) => {
            eprintln!("skip: guest wasm present — NotBuilt path not exercised");
        }
        Err(e) => panic!("unexpected load error: {e:?}"),
    }
}
