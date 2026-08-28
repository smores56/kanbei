//! S4 spike benches: manifest materialization/dedup over a simulated pinned
//! history; install-protocol cost (plain vs batched dirsync); closure verify;
//! object-store scale to 1M files; prune scan.
//! Disposable spike code — never promoted into the implementation.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kb_s4_snapshot::{digest, pin, prune_scan, verify_closure, Manifest, ObjectStore};

fn tmp(sub: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-s4-{sub}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn pct(times: &mut [Duration], p: f64) -> Duration {
    times.sort();
    let idx = ((times.len() as f64) * p).floor() as usize;
    times[idx.min(times.len() - 1)]
}

fn report(tag: &str, times: &mut Vec<Duration>) {
    times.sort();
    println!("{tag}: n={} avg={:?} p50={:?} p99={:?} max={:?}", times.len(),
        times.iter().sum::<Duration>() / times.len() as u32, pct(times, 0.5), pct(times, 0.99), *times.last().unwrap());
}

// ---------- simulated pinned history ----------

fn bench_history(events: u64, state_change_every: u64, batched: bool) {
    let dir = tmp("history");
    let mut store = ObjectStore::open(&dir, batched).unwrap();
    let mut state = 0u64;
    let mut pins = 0u64;
    let mut created = 0u64;
    let mut dedup = 0u64;
    let mut pin_times = Vec::new();
    let mut last_manifest = String::new();
    let mut closure_objs: HashSet<String> = HashSet::new();
    let t0 = Instant::now();
    for seq in 0..events {
        // state-changing transitions: every state_change_every events
        if seq % state_change_every == 0 {
            state += 1;
        }
        // pin at state-changing transitions + run genesis (every 1000) + policy changes (every 10k)
        let is_transition = seq % state_change_every == 0;
        let is_genesis = seq % 1000 == 0;
        let is_policy = seq % 10_000 == 0;
        if is_transition || is_genesis || is_policy {
            let m = Manifest {
                schema: 1,
                state_head: format!("{:x}", state),
                memory_root: format!("{:x}", seq / 2000),
                tool_registry: 3,
                projection: 2,
                provider: 1,
                policy: if is_policy { 1 } else { 0 },
                schema_versions: vec![1],
            };
            let t = Instant::now();
            let (d, new) = pin(&mut store, &m).unwrap();
            pin_times.push(t.elapsed());
            closure_objs.insert(format!("state:{}", m.state_head));
            closure_objs.insert(format!("memory:{}", m.memory_root));
            closure_objs.insert(format!("policy:{}", m.policy));
            closure_objs.insert(d.clone());
            if new {
                created += 1;
            } else {
                dedup += 1;
            }
            pins += 1;
            last_manifest = d;
        }
    }
    if batched {
        store.dirsync().unwrap();
    }
    let total = t0.elapsed();
    println!("== history events={events} state_change_every={state_change_every} batched_dirsync={batched}");
    println!("  pins={pins} manifests_created={created} deduped={dedup} dedup_ratio={:.3}", dedup as f64 / pins as f64);
    println!("  closure: {} unique referenced objects for {pins} pins (closure/pins={:.3})", closure_objs.len(), closure_objs.len() as f64 / pins as f64);
    println!("  total={total:?} pin_cost_avg={:?}", total / pins as u32);
    report("  pin_latency", &mut pin_times);
    println!("  last_manifest_digest={last_manifest} bytes={}", store.get(&last_manifest).unwrap().len());
}

// ---------- closure verify ----------

fn bench_closure(batched: bool, manifests: u64) {
    let dir = tmp("closure");
    let mut store = ObjectStore::open(&dir, batched).unwrap();
    let mut refs = Vec::new();
    for i in 0..manifests {
        let m = Manifest {
            schema: 1,
            state_head: format!("{i:x}"),
            memory_root: "m".into(),
            tool_registry: 3,
            projection: 2,
            provider: 1,
            policy: 0,
            schema_versions: vec![1],
        };
        let (d, _) = pin(&mut store, &m).unwrap();
        refs.push(d);
    }
    if batched {
        store.dirsync().unwrap();
    }
    let mut times = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let n = verify_closure(&store, &refs).unwrap();
        times.push(t.elapsed());
        assert_eq!(n, manifests);
    }
    report(&format!("closure_verify {manifests} manifests (hash-check each)"), &mut times);
}

// ---------- object-store scale ----------

fn bench_scale(n: u64, batched: bool, size: usize, no_fsync: bool) {
    let dir = tmp("scale");
    let mut store = ObjectStore::open(&dir, batched).unwrap();
    let mut digests = Vec::with_capacity(n as usize);
    let t0 = Instant::now();
    for i in 0..n {
        let bytes = format!("obj-{i}-{}", "x".repeat(size));
        let d = if no_fsync {
            // write+rename only (durability queue does fsync off-path)
            let d = digest(bytes.as_bytes());
            let dst = dir.join(format!("blake3:{d}"));
            if !dst.exists() {
                std::fs::write(&dst, &bytes).unwrap();
            }
            d
        } else {
            store.install(bytes.as_bytes()).unwrap()
        };
        digests.push(d);
    }
    if batched && !no_fsync {
        store.dirsync().unwrap();
    }
    let install_total = t0.elapsed();
    println!("== scale n={n} size={size}B batched_dirsync={batched} no_fsync={no_fsync}: install_total={install_total:?} ({:.0} obj/s, {:.0} us/obj), dirsyncs={}",
        n as f64 / install_total.as_secs_f64(), install_total.as_micros() as f64 / n as f64, store.dirsyncs);

    // random reads with hash verify
    let mut times = Vec::new();
    let mut idx = 0usize;
    for _ in 0..10_000 {
        idx = (idx * 7 + 13) % n as usize;
        let t = Instant::now();
        let bytes = store.get(&digests[idx]).unwrap();
        times.push(t.elapsed());
        assert!(bytes.starts_with(b"obj-"));
    }
    report("  random_read+verify", &mut times);

    // list
    let t = Instant::now();
    let listed = store.scan().unwrap();
    let list_time = t.elapsed();
    println!("  list: {} entries in {list_time:?}", listed.len());

    // prune scan with a referenced set of half the objects
    let t = Instant::now();
    let referenced: HashSet<String> = digests.iter().step_by(2).cloned().collect();
    let (orphans, total) = prune_scan(&store, &referenced).unwrap();
    let prune_time = t.elapsed();
    println!("  prune_scan: {orphans} orphans of {total} in {prune_time:?}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let batched = args.iter().any(|a| a == "--batched");
    match args.get(1).map(|s| s.as_str()).unwrap_or("") {
        "history" => {
            let events: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100_000);
            let every: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
            bench_history(events, every, batched);
        }
        "closure" => {
            let n: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            bench_closure(batched, n);
        }
        "scale" => {
            let n: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100_000);
            let size: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(256);
            let no_fsync = args.iter().any(|a| a == "--no-fsync");
            bench_scale(n, batched, size, no_fsync);
        }
        _ => {
            eprintln!("usage: kb-s4-snapshot <history [events] [state_change_every]|closure [n]|scale [n] [size]> [--batched] [--no-fsync]");
        }
    }
}
