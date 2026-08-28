//! S1 spike host harness: drives the wasm32-wasip1 Luaur guest through wasmtime
//! and measures the numbers the hosting fallback tree needs.
//! Disposable spike code — never promoted into the implementation.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use wasmtime::{Caller, Config, Engine, Error, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc};
use wasmtime_wasi::p1::{self as wasi_p1, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

const WASM_REL: &str = "target/wasm32-wasip1/release/kb_guest.wasm";

const TRIVIAL: &str = "return 1";
const BUSY: &str = "local x = 0 for i = 1, 1000000000 do x = x + i end";
const ALLOC: &str = "local t = {} for i = 1, 10000000 do t[i] = i end";

fn config_source() -> String {
    let mut s = String::from("local cfg = {\n");
    for i in 0..200 {
        s.push_str(&format!("  key{i} = \"value-{i}-of-a-configuration-entry\",\n"));
    }
    s.push_str("}\nreturn cfg\n");
    s
}

struct Ctx {
    limits: StoreLimits,
    wasi: WasiP1Ctx,
}

struct Exports {
    run: TypedFunc<(i32, i32), i32>,
    compile: TypedFunc<(i32, i32), i32>,
    init: TypedFunc<(i32, i32), i32>,
    hot_call: TypedFunc<i32, i32>,
    scratch: TypedFunc<(), i32>,
    memory: Memory,
}

struct Harness {
    engine: Engine,
    module: Module,
}

impl Harness {
    fn load() -> Result<Self, Error> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg)?;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(WASM_REL);
        let module = Module::from_file(&engine, &path)?;
        Ok(Self { engine, module })
    }

    async fn instantiate(&self, async_store: bool, mem_limit: Option<usize>) -> Result<(Store<Ctx>, Exports), Error> {
        let limits = match mem_limit {
            Some(bytes) => StoreLimitsBuilder::new().memory_size(bytes).build(),
            None => StoreLimits::default(),
        };
        let ctx = Ctx {
            limits,
            wasi: WasiCtxBuilder::new().build_p1(),
        };

        let mut store = Store::new(&self.engine, ctx);
        // consume_fuel is on globally; benches that care override this later.
        store.set_fuel(u64::MAX)?;
        // epoch_interruption is on globally; deadline 0 traps immediately.
        store.set_epoch_deadline(u64::MAX);
        // StoreLimits only implements the sync ResourceLimiter in wasmtime 48;
        // async benches (roundtrip) don't exercise memory limits.
        if !async_store {
            store.limiter(|c| &mut c.limits);
        }

        let mut linker = Linker::new(&self.engine);
        if async_store {
            wasi_p1::add_to_linker_async(&mut linker, |c: &mut Ctx| &mut c.wasi)?;
            linker.func_wrap_async("env", "kb_host", |_caller: Caller<'_, Ctx>, (op, x): (i32, i32)| {
                Box::new(async move {
                    Ok::<i32, Error>(match op {
                        0 => x * 2,
                        1 => x * 3,
                        _ => -1,
                    })
                })
            })?;
        } else {
            wasi_p1::add_to_linker_sync(&mut linker, |c: &mut Ctx| &mut c.wasi)?;
            linker.func_wrap("env", "kb_host", |op: i32, x: i32| match op {
                0 => x * 2,
                1 => x * 3,
                _ => -1,
            })?;
        }
        let instance = if async_store {
            linker.instantiate_async(&mut store, &self.module).await?
        } else {
            linker.instantiate(&mut store, &self.module)?
        };

        let exports = Exports {
            run: instance.get_typed_func(&mut store, "kb_run")?,
            compile: instance.get_typed_func(&mut store, "kb_compile")?,
            init: instance.get_typed_func(&mut store, "kb_init")?,
            hot_call: instance.get_typed_func(&mut store, "kb_hot_call")?,
            scratch: instance.get_typed_func(&mut store, "kb_scratch")?,
            memory: instance.get_memory(&mut store, "memory").expect("exported memory"),
        };
        Ok((store, exports))
    }

    async fn write_src(&self, store: &mut Store<Ctx>, exports: &Exports, src: &str, async_store: bool) -> Result<(i32, i32), Error> {
        let base = if async_store {
            exports.scratch.call_async(&mut *store, ()).await?
        } else {
            exports.scratch.call(&mut *store, ())?
        } as usize;
        exports.memory.write(&mut *store, base, src.as_bytes())?;
        Ok((base as i32, src.len() as i32))
    }
}

fn pct(sorted: &mut [Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64) * p).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn report(tag: &str, times: &mut [Duration]) {
    times.sort();
    println!("{tag}: avg={:?} p50={:?} p99={:?} min={:?} max={:?}",
        times.iter().sum::<Duration>() / times.len() as u32,
        pct(times, 0.5), pct(times, 0.99), times[0], *times.last().unwrap());
}

async fn bench_cold(h: &Harness) -> Result<(), Error> {
    let t0 = Instant::now();
    let (mut store, exports) = h.instantiate(false, None).await?;
    let (ptr, len) = h.write_src(&mut store, &exports, TRIVIAL, false).await?;
    let t1 = Instant::now();
    let code = exports.run.call(&mut store, (ptr, len))?;
    assert_eq!(code, 0, "guest returned {code}");
    let t2 = Instant::now();
    println!("cold_total (engine+module+store+instance+first call): {:?}", t2 - t0);
    println!("cold_instantiate: {:?}", t1 - t0);
    println!("cold_first_call (compile+VM+run): {:?}", t2 - t1);

    let mut calls = Vec::new();
    for _ in 0..10 {
        let (mut store, exports) = h.instantiate(false, None).await?;
        let (ptr, len) = h.write_src(&mut store, &exports, TRIVIAL, false).await?;
        let t = Instant::now();
        exports.run.call(&mut store, (ptr, len))?;
        calls.push(t.elapsed());
    }
    report("instance_cold_calls", &mut calls);
    Ok(())
}

async fn bench_compile(h: &Harness) -> Result<(), Error> {
    let cfg = config_source();
    let (mut store, exports) = h.instantiate(false, None).await?;
    let (ptr, len) = h.write_src(&mut store, &exports, &cfg, false).await?;
    let mut times = Vec::new();
    for _ in 0..100 {
        let t = Instant::now();
        let code = exports.compile.call(&mut store, (ptr, len))?;
        assert_eq!(code, 0);
        times.push(t.elapsed());
    }
    println!("config_compile ({} B source):", cfg.len());
    report("  compile_in_guest", &mut times);
    Ok(())
}

async fn bench_hot(h: &Harness) -> Result<(), Error> {
    let (mut store, exports) = h.instantiate(false, None).await?;
    let src = "function kb_hot(x) return x * 2 end";
    let (ptr, len) = h.write_src(&mut store, &exports, src, false).await?;
    let code = exports.init.call(&mut store, (ptr, len))?;
    assert_eq!(code, 0, "kb_init returned {code}");
    // warm up, then time N host->guest calls of the cached function
    for _ in 0..1000 {
        exports.hot_call.call(&mut store, 1)?;
    }
    let n = 1_000_000i64;
    let t = Instant::now();
    for _ in 0..n {
        exports.hot_call.call(&mut store, 1)?;
    }
    let total = t.elapsed();
    println!("hot_call (host->guest, cached fn, sync): n={n} total={total:?} per_call={:?}", total / n as u32);
    Ok(())
}

async fn bench_roundtrip(h: &Harness, async_store: bool) -> Result<(), Error> {
    let n = 100_000i64;
    let script = format!("local n = {n} for i = 1, n do kb_host_{}(i) end", if async_store { "async" } else { "double" });
    let (mut store, exports) = h.instantiate(async_store, None).await?;
    let (ptr, len) = h.write_src(&mut store, &exports, &script, async_store).await?;
    let t = Instant::now();
    let code = if async_store {
        exports.run.call_async(&mut store, (ptr, len)).await?
    } else {
        exports.run.call(&mut store, (ptr, len))?
    };
    assert_eq!(code, 0);
    let total = t.elapsed();
    println!("host_call_roundtrip mode={} n={n}: total={:?} per_call={:?}",
        if async_store { "async" } else { "sync" }, total, total / n as u32);
    Ok(())
}

async fn bench_fuel(h: &Harness, fuel: u64) -> Result<(), Error> {
    let (mut store, exports) = h.instantiate(false, None).await?;
    store.set_fuel(fuel)?;
    let (ptr, len) = h.write_src(&mut store, &exports, BUSY, false).await?;
    let t = Instant::now();
    let res = exports.run.call(&mut store, (ptr, len));
    let elapsed = t.elapsed();
    match res {
        Err(e) if is_interrupt(&e) => {
            let remaining = store.get_fuel().unwrap_or(0);
            println!("fuel: trip after {elapsed:?}, consumed={} of {fuel}", fuel - remaining);
        }
        Err(e) => println!("fuel: unexpected error {e:?} after {elapsed:?}"),
        Ok(code) => println!("fuel: completed code={code} (no trip)"),
    }
    Ok(())
}

async fn bench_epoch(h: &Harness) -> Result<(), Error> {
    let engine = h.engine.clone();
    let bump = {
        let engine = engine.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
            engine.increment_epoch();
        })
    };
    let (mut store, exports) = h.instantiate(false, None).await?;
    store.set_epoch_deadline(1);
    let (ptr, len) = h.write_src(&mut store, &exports, BUSY, false).await?;
    let t = Instant::now();
    let res = exports.run.call(&mut store, (ptr, len));
    let elapsed = t.elapsed();
    match res {
        Err(e) if is_interrupt(&e) => println!("epoch: trip after {elapsed:?} (bump every 10ms)"),
        Err(e) => println!("epoch: unexpected error {e:?} after {elapsed:?}"),
        Ok(code) => println!("epoch: completed code={code} (no trip)"),
    }
    drop(bump);
    Ok(())
}

async fn bench_limits(h: &Harness, mem_limit: usize) -> Result<(), Error> {
    let (mut store, exports) = h.instantiate(false, Some(mem_limit)).await?;
    let (ptr, len) = h.write_src(&mut store, &exports, ALLOC, false).await?;
    let res = exports.run.call(&mut store, (ptr, len));
    match res {
        Err(e) => println!("limits: mem_limit={} trap={:?}", mem_limit, trap_kind(&e)),
        Ok(code) => println!("limits: mem_limit={} completed code={code} (no trap)", mem_limit),
    }
    Ok(())
}

async fn bench_respawn(h: &Harness, fuel: u64) -> Result<(), Error> {
    let mut cycles = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let (mut store, exports) = h.instantiate(false, None).await?;
        store.set_fuel(fuel)?;
        let (ptr, len) = h.write_src(&mut store, &exports, BUSY, false).await?;
        let res = exports.run.call(&mut store, (ptr, len));
        let elapsed = t.elapsed();
        assert!(matches!(res, Err(ref e) if is_interrupt(e)), "expected fuel trip, got {res:?}");
        drop(store);
        cycles.push(elapsed);
    }
    report("respawn (fuel trip, module cached)", &mut cycles);
    Ok(())
}

fn is_interrupt(e: &Error) -> bool {
    matches!(trap_kind(e), TrapKind::Interrupt | TrapKind::Fuel)
}

#[derive(Debug)]
enum TrapKind {
    Interrupt,
    Fuel,
    Oom,
    Other(String),
}

fn trap_kind(e: &Error) -> TrapKind {
    if let Some(t) = e.downcast_ref::<Trap>() {
        match t {
            Trap::Interrupt => TrapKind::Interrupt,
            Trap::OutOfFuel => TrapKind::Fuel,
            other => TrapKind::Other(format!("{other:?}")),
        }
    } else if e.downcast_ref::<wasmtime::OutOfMemory>().is_some() {
        TrapKind::Oom
    } else {
        TrapKind::Other(format!("{e}"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let harness = Harness::load()?;
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "cold" => bench_cold(&harness).await?,
        "compile" => bench_compile(&harness).await?,
        "hot" => bench_hot(&harness).await?,
        "roundtrip" => {
            let mode = std::env::args().nth(2).unwrap_or_else(|| "sync".into());
            bench_roundtrip(&harness, mode == "async").await?
        }
        "fuel" => {
            let fuel: u64 = std::env::args().nth(2).unwrap_or_else(|| "100000".into()).parse().unwrap();
            bench_fuel(&harness, fuel).await?
        }
        "epoch" => bench_epoch(&harness).await?,
        "limits" => {
            let mb: usize = std::env::args().nth(2).unwrap_or_else(|| "8".into()).parse().unwrap();
            bench_limits(&harness, mb * 1024 * 1024).await?
        }
        "respawn" => {
            let fuel: u64 = std::env::args().nth(2).unwrap_or_else(|| "100000".into()).parse().unwrap();
            bench_respawn(&harness, fuel).await?
        }
        other => {
            eprintln!("usage: kb-host <cold|compile|hot|roundtrip [sync|async]|fuel [n]|epoch|limits [mb]|respawn [fuel]>");
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
    Ok(())
}
