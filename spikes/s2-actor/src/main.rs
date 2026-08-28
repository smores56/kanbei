//! S2 spike: session-actor throughput under mixed command/outcome + wake chain + chunk commits.
//! Disposable spike code — never promoted into the implementation.
//!
//! Models the session-actor commit path: serialized command processing (responder
//! lane prioritized over outcomes), one canonical AppendLog writer (zstd frames,
//! durability profiles), and an optional SQLite projection insert on the commit
//! path. Budget under test (architecture.md R-21/H-04): event-commit ACK p99
//! <= 10 ms at >= 100 events/s sustained.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use rusqlite::Connection;

const FRAME_LEVEL: i32 = 3;

#[derive(Clone, Copy, PartialEq)]
enum Profile {
    Fast,
    Balanced,
    Strict,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Balanced => "balanced",
            Profile::Strict => "strict",
        }
    }
    fn from(s: &str) -> Self {
        match s {
            "fast" => Profile::Fast,
            "strict" => Profile::Strict,
            _ => Profile::Balanced,
        }
    }
}

fn event(seq: u64, kind: &str) -> String {
    format!("{{\"seq\":{seq},\"kind\":\"{kind}\",\"run\":\"run_demo\"}}")
}

// ---------- AppendLog-lite ----------

struct Log {
    file: File,
    profile: Profile,
    since: u64,
    frames: u64,
    fsyncs: u64,
    bytes: u64,
}

impl Log {
    fn new(path: &str, profile: Profile) -> std::io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
            profile,
            since: 0,
            frames: 0,
            fsyncs: 0,
            bytes: 0,
        })
    }

    /// One zstd frame per commit; events are JSONL records inside the frame.
    fn commit(&mut self, events: &[String]) -> std::io::Result<()> {
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), FRAME_LEVEL)?;
        for e in events {
            writeln!(enc, "{e}")?;
        }
        let frame = enc.finish()?;
        self.file.write_all(&frame)?;
        self.bytes += frame.len() as u64;
        self.frames += 1;
        match self.profile {
            Profile::Fast => {}
            Profile::Strict => {
                self.file.sync_all()?;
                self.fsyncs += 1;
            }
            Profile::Balanced => {
                self.since += 1;
                if self.since >= 10 {
                    self.since = 0;
                    self.file.sync_all()?;
                    self.fsyncs += 1;
                }
            }
        }
        Ok(())
    }
}

// ---------- commands ----------

#[derive(Debug)]
enum Cmd {
    UserMsg { sent: Instant },
    Outcome { sent: Instant, delay_ms: u64 },
    Chunk { sent: Instant, n: usize },
    Wake { sent: Instant, delay_ms: u64 },
    Shutdown,
}

struct Stat {
    kind: &'static str,
    sent: Instant,
    committed: Instant,
}

// ---------- actor ----------

struct Actor {
    log: Log,
    db: Option<Connection>,
    seq: u64,
    user_rx: Receiver<Cmd>,
    out_rx: Receiver<Cmd>,
    stats: Sender<Stat>,
}

impl Actor {
    fn commit(&mut self, kind: &'static str, sent: Instant, n: usize) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let events: Vec<(i64, String)> = (0..n)
            .map(|_| {
                self.seq += 1;
                (self.seq as i64, event(self.seq, kind))
            })
            .collect();
        let payloads: Vec<String> = events.iter().map(|(_, e)| e.clone()).collect();
        self.log.commit(&payloads)?;
        if let Some(db) = &mut self.db {
            db.execute_batch("BEGIN")?;
            for (seq, payload) in &events {
                db.execute("INSERT INTO events (seq, payload) VALUES (?1, ?2)", rusqlite::params![seq, payload])?;
            }
            db.execute_batch("COMMIT")?;
        }
        self.stats.send(Stat { kind, sent, committed: Instant::now() }).ok();
        Ok(())
    }

    fn handle(&mut self, cmd: Cmd, out_tx: &Sender<Cmd>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match cmd {
            Cmd::UserMsg { sent } => self.commit("user_message", sent, 1)?,
            Cmd::Chunk { sent, n } => self.commit("tool_chunk", sent, n)?,
            Cmd::Wake { sent, delay_ms } => {
                self.commit("wake_accepted", sent, 1)?;
                spawn_outcome(out_tx, delay_ms);
            }
            Cmd::Outcome { sent, delay_ms } => {
                self.commit("model_outcome", sent, 1)?;
                // perpetual chain: accept the next wake in the same turn
                self.commit("wake_accepted", sent, 1)?;
                spawn_outcome(out_tx, delay_ms);
            }
            Cmd::Shutdown => {}
        }
        Ok(())
    }

    fn run(mut self, out_tx: Sender<Cmd>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // responder lane first: drain user commands exhaustively before any
        // outcome; block on the outcome lane only when the user lane is empty.
        loop {
            while let Ok(cmd) = self.user_rx.try_recv() {
                if matches!(cmd, Cmd::Shutdown) {
                    return Ok(());
                }
                self.handle(cmd, &out_tx)?;
            }
            match self.out_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(cmd) => {
                    if matches!(cmd, Cmd::Shutdown) {
                        return Ok(());
                    }
                    self.handle(cmd, &out_tx)?;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        Ok(())
    }
}

fn spawn_outcome(out_tx: &Sender<Cmd>, delay_ms: u64) {
    let tx = out_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        tx.send(Cmd::Outcome { sent: Instant::now(), delay_ms }).ok();
    });
}

// ---------- load generators ----------

fn make_actor(profile: Profile, with_sqlite: bool) -> (Actor, Sender<Cmd>, Sender<Cmd>, Receiver<Stat>) {
    let log_path = std::env::temp_dir().join("kb-s2-events.jsonl.zst");
    let log = Log::new(log_path.to_str().unwrap(), profile).expect("log");
    let db = if with_sqlite {
        let conn = Connection::open_in_memory().expect("sqlite");
        conn.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, payload TEXT)").expect("schema");
        Some(conn)
    } else {
        None
    };
    let (user_tx, user_rx) = bounded::<Cmd>(1 << 16);
    let (out_tx, out_rx) = unbounded::<Cmd>();
    let (stats_tx, stats_rx) = unbounded();
    let actor = Actor { log, db, seq: 0, user_rx, out_rx, stats: stats_tx };
    (actor, user_tx, out_tx, stats_rx)
}

fn pct(times: &mut [Duration], p: f64) -> Duration {
    times.sort();
    let idx = ((times.len() as f64) * p).floor() as usize;
    times[idx.min(times.len() - 1)]
}

fn report(tag: &str, times: &mut Vec<Duration>) {
    if times.is_empty() {
        return;
    }
    println!("{tag}: n={} avg={:?} p50={:?} p99={:?} max={:?}", times.len(),
        times.iter().sum::<Duration>() / times.len() as u32, pct(times, 0.5), pct(times, 0.99), *times.iter().max().unwrap());
}

fn collect(stats: &Receiver<Stat>, by_kind: &mut std::collections::HashMap<&'static str, Vec<Duration>>) {
    while let Ok(s) = stats.try_recv() {
        by_kind.entry(s.kind).or_default().push(s.committed - s.sent);
    }
}

fn drain_stats(stats: &Receiver<Stat>) -> std::collections::HashMap<&'static str, Vec<Duration>> {
    let mut by_kind: std::collections::HashMap<&'static str, Vec<Duration>> = Default::default();
    collect(stats, &mut by_kind);
    by_kind
}

fn run_sustained(profile: Profile, with_sqlite: bool, rate: u64, secs: u64) {
    let (actor, user_tx, out_tx, stats_rx) = make_actor(profile, with_sqlite);
    let thread_tx = out_tx.clone();
    let handle = std::thread::spawn(move || actor.run(thread_tx));
    let mut by_kind = drain_stats(&stats_rx);
    let mut sent = 0u64;
    let mut events = 0u64;
    let start = Instant::now();
    let interval = Duration::from_micros(1_000_000 / rate);
    while start.elapsed() < Duration::from_secs(secs) {
        let t = Instant::now();
        // mixed load: responder msgs and chunk commits (no wake threads)
        if sent % 3 == 0 {
            user_tx.send(Cmd::UserMsg { sent: t }).unwrap();
            events += 1;
        } else {
            out_tx.send(Cmd::Chunk { sent: t, n: 4 }).unwrap();
            events += 4;
        }
        sent += 1;
        collect(&stats_rx, &mut by_kind);
        std::thread::sleep(interval);
    }
    std::thread::sleep(Duration::from_millis(200));
    collect(&stats_rx, &mut by_kind);
    user_tx.send(Cmd::Shutdown).unwrap();
    handle.join().unwrap().unwrap();
    let elapsed = start.elapsed();
    println!("== sustained target={rate}/s profile={} sqlite={with_sqlite} wall={elapsed:?} cmds={sent} events={events} achieved={:.0} ev/s", profile.name(), events as f64 / elapsed.as_secs_f64());
    for (k, v) in &mut by_kind {
        report(&format!("  {k}"), v);
    }
}

fn run_burst(profile: Profile, with_sqlite: bool, n: u64) {
    let (actor, user_tx, out_tx, stats_rx) = make_actor(profile, with_sqlite);
    let thread_tx = out_tx.clone();
    let handle = std::thread::spawn(move || actor.run(thread_tx));
    let start = Instant::now();
    for i in 0..n {
        let t = Instant::now();
        if i % 3 == 0 {
            user_tx.send(Cmd::UserMsg { sent: t }).unwrap();
        } else {
            out_tx.send(Cmd::Chunk { sent: t, n: 4 }).unwrap();
        }
    }
    user_tx.send(Cmd::Shutdown).unwrap();
    handle.join().unwrap().unwrap();
    let total = start.elapsed();
    let by_kind = drain_stats(&stats_rx);
    let events: u64 = by_kind.values().map(|v| v.len() as u64).sum();
    println!("== burst cmds={n} profile={} sqlite={with_sqlite}: drain={:?} throughput={:.0} ev/s", profile.name(), total, events as f64 / total.as_secs_f64());
    let mut all = Vec::new();
    for (k, mut v) in by_kind {
        all.append(&mut v);
        report(&format!("  {k}"), &mut v);
    }
    report("  all_commits", &mut all);
}

fn run_wakechain(profile: Profile, with_sqlite: bool, delay_ms: u64, cycles: u64) {
    let (actor, user_tx, out_tx, stats_rx) = make_actor(profile, with_sqlite);
    let thread_tx = out_tx.clone();
    let handle = std::thread::spawn(move || actor.run(thread_tx));
    let mut wake_times = Vec::new();
    let start = Instant::now();
    let mut last = start;
    let mut count = 0u64;
    // seed the chain via the outcome lane (same path a scheduler wake uses)
    out_tx.send(Cmd::Wake { sent: Instant::now(), delay_ms }).unwrap();
    while count < cycles {
        while let Ok(s) = stats_rx.try_recv() {
            if s.kind == "wake_accepted" {
                wake_times.push(s.committed - last);
                last = s.committed;
                count += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    user_tx.send(Cmd::Shutdown).unwrap();
    handle.join().unwrap().unwrap();
    println!("== wakechain delay={delay_ms}ms cycles={cycles} profile={} sqlite={with_sqlite}: wake-to-wake interval (delay + 2 commits)", profile.name());
    report("  wake_interval", &mut wake_times);
}

fn run_priority(profile: Profile, with_sqlite: bool, pending: u64, delay_ms: u64) {
    let (actor, user_tx, out_tx, stats_rx) = make_actor(profile, with_sqlite);
    let thread_tx = out_tx.clone();
    let handle = std::thread::spawn(move || actor.run(thread_tx));
    // flood the outcome lane with pending completions, then send one user msg
    for _ in 0..pending {
        out_tx.send(Cmd::Outcome { sent: Instant::now(), delay_ms }).unwrap();
    }
    std::thread::sleep(Duration::from_millis(50)); // let the actor start draining
    let t = Instant::now();
    user_tx.send(Cmd::UserMsg { sent: t }).unwrap();
    let mut user_lat = None;
    while user_lat.is_none() {
        while let Ok(s) = stats_rx.try_recv() {
            if s.kind == "user_message" {
                user_lat = Some(s.committed - s.sent);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    user_tx.send(Cmd::Shutdown).unwrap();
    handle.join().unwrap().unwrap();
    let by_kind = drain_stats(&stats_rx);
    println!("== priority pending={pending} delay={delay_ms}ms profile={} sqlite={with_sqlite}: user_msg_latency={:?}", profile.name(), user_lat.unwrap());
    for (k, mut v) in by_kind {
        report(&format!("  {k}"), &mut v);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let profile = Profile::from(args.get(2).map(|s| s.as_str()).unwrap_or("balanced"));
    let with_sqlite = !args.iter().any(|a| a == "--no-sqlite");
    match cmd {
        "sustained" => {
            let rate: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
            let secs: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);
            run_sustained(profile, with_sqlite, rate, secs);
        }
        "burst" => {
            let n: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            run_burst(profile, with_sqlite, n);
        }
        "wakechain" => {
            let delay: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
            let cycles: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(100);
            run_wakechain(profile, with_sqlite, delay, cycles);
        }
        "priority" => {
            let pending: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
            let delay: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);
            run_priority(profile, with_sqlite, pending, delay);
        }
        _ => {
            eprintln!("usage: kb-s2-actor <sustained|burst|wakechain|priority> [profile fast|balanced|strict] [args...] [--no-sqlite]");
            eprintln!("  sustained <rate> <secs> | burst <n> | wakechain <delay_ms> <cycles> | priority <pending> <delay_ms>");
        }
    }
}
