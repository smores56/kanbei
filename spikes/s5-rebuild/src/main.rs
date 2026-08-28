//! S5 spike: projection rebuild throughput and memory over large canonical
//! streams. Budgets: >= 10k events/s streaming rebuild; 5M-event stream
//! <= 15 min and < 512 MB RSS.
//! Disposable spike code — never promoted into the implementation.

use std::path::{Path, PathBuf};
use std::time::Instant;

use kb_s3_appendlog::{for_each_frame, LogWriter, Profile};
use rusqlite::Connection;

fn event(seq: u64) -> String {
    format!("{{\"seq\":{seq},\"kind\":\"user_message\",\"run\":\"run_demo\",\"text\":\"hello world this is a canonical event payload\"}}")
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-s5-{name}"));
    let _ = std::fs::remove_file(&p);
    p
}

fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("VmHWM:") {
            return v.trim().trim_end_matches(" kB").parse().unwrap_or(0);
        }
    }
    0
}

fn generate(path: &Path, events: u64, per_frame: usize) {
    let mut w = LogWriter::open(path, "demo").unwrap();
    let t = Instant::now();
    let mut done = 0u64;
    while done < events {
        let batch: Vec<String> = (0..per_frame).map(|_| event(w.seq())).collect();
        w.append_frame(&batch, Profile::Fast).unwrap();
        done += batch.len() as u64;
    }
    let elapsed = t.elapsed();
    println!("gen: {events} events x {per_frame}/frame -> {} ({} B) in {elapsed:?} ({:.0} ev/s)",
        path.display(), w.bytes_written, events as f64 / elapsed.as_secs_f64());
}

fn rebuild(path: &Path, sqlite: Option<&Path>, tx_batch: u64) {
    let mut conn = match sqlite {
        Some(p) => {
            let c = Connection::open(p).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.pragma_update(None, "synchronous", "OFF").unwrap();
            c
        }
        None => Connection::open_in_memory().unwrap(),
    };
    conn.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, kind TEXT, payload TEXT, run TEXT)").unwrap();
    let mut stmt = conn.prepare("INSERT INTO events (seq, kind, payload, run) VALUES (?1, ?2, ?3, ?4)").unwrap();
    conn.execute_batch("BEGIN").unwrap();

    let t0 = Instant::now();
    let mut inserted = 0u64;
    let mut in_tx = 0u64;
    let result = for_each_frame(path, |frame| {
        for e in frame.events {
            let seq: i64 = inserted as i64;
            let kind = if e.contains("\"kind\":\"user_message\"") { "user_message" } else { "other" };
            stmt.execute(rusqlite::params![seq, kind, e, "run_demo"]).unwrap();
            inserted += 1;
            in_tx += 1;
            if in_tx >= tx_batch {
                conn.execute_batch("COMMIT; BEGIN").unwrap();
                in_tx = 0;
            }
        }
        Ok(())
    });
    let rec = result.unwrap();
    conn.execute_batch("COMMIT").ok();
    let elapsed = t0.elapsed();
    drop(stmt);
    let count: u64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    let mode = match sqlite {
        Some(p) => format!("file({}, WAL, synchronous=OFF)", p.display()),
        None => "memory".to_string(),
    };
    println!("rebuild: mode={mode} frames={} events={} inserted={inserted} db_count={count} in {elapsed:?} ({:.0} ev/s) RSS={} kB",
        rec.frames, rec.events, inserted as f64 / elapsed.as_secs_f64(), rss_kb());
    assert_eq!(inserted, count);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()).unwrap_or("") {
        "gen" | "generate" => {
            let path = PathBuf::from(args.get(2).expect("path"));
            let events: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
            let per: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(64);
            generate(&path, events, per);
        }
        "rebuild" => {
            let path = PathBuf::from(args.get(2).expect("path"));
            let mode = args.get(3).map(|s| s.as_str()).unwrap_or("memory");
            let tx: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1000);
            let sqlite = if mode == "file" { Some(tmp("projection.sqlite")) } else { None };
            rebuild(&path, sqlite.as_deref(), tx);
        }
        _ => {
            eprintln!("usage: kb-s5-rebuild <gen <path> <events> [per_frame]|rebuild <path> [memory|file] [tx_batch]>");
        }
    }
}
