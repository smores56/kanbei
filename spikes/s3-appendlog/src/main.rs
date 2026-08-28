//! S3 spike benches: framing sweep, chain verify, torn-tail/corruption drills,
//! kill -9 durability, dirsync cost, fsync-off-critical-path.
//! Disposable spike code — never promoted into the implementation.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Sender};
use kb_s3_appendlog::{export, recover, scan_frames, LogWriter, Profile};

fn event(seq: u64) -> String {
    format!("{{\"seq\":{seq},\"kind\":\"user_message\",\"run\":\"run_demo\",\"text\":\"hello world this is a canonical event payload\"}}")
}

fn tmp(path: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-s3-{path}"));
    let _ = std::fs::remove_file(&p);
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

// ---------- sweep: level x events-per-frame ----------

fn bench_sweep() {
    let path = tmp("sweep.jsonl.zst");
    let mut raw = 0u64;
    for level in [1i32, 3, 6, 9, 19] {
        for per_frame in [1usize, 8, 64, 1024] {
            let mut w = LogWriter::open(&path, "demo").unwrap();
            let n = 100_000u64;
            let mut seq0 = w.seq();
            let t = Instant::now();
            let mut frames = 0u64;
            let mut events_written = 0u64;
            while events_written < n {
                let batch: Vec<String> = (0..per_frame).map(|_| {
                    let s = seq0;
                    seq0 += 1;
                    event(s)
                }).collect();
                w.append_frame(&batch, Profile::Fast).unwrap();
                frames += 1;
                events_written += batch.len() as u64;
            }
            let elapsed = t.elapsed();
            let _ = std::fs::remove_file(&path);
            // raw JSONL size for amplification
            if level == 1 {
                raw = (0..n).map(|i| event(i).len() as u64 + 1).sum();
            }
            println!("sweep level={level} events/frame={per_frame}: {:?} total, {:.1} us/event, {:.1} bytes/event (raw {:.1}), amp {:.2}x, {} frames",
                elapsed, elapsed.as_micros() as f64 / n as f64,
                w.bytes_written as f64 / n as f64, raw as f64 / n as f64, w.bytes_written as f64 / raw as f64, frames);
        }
    }
}

// ---------- chain verify ----------

fn bench_verify(frames: u64) {
    let path = tmp("verify.jsonl.zst");
    let mut w = LogWriter::open(&path, "demo").unwrap();
    let per = 100usize;
    for _ in 0..frames {
        let batch: Vec<String> = (0..per).map(|_| event(w.seq())).collect();
        w.append_frame(&batch, Profile::Fast).unwrap();
    }
    let mut times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let (rec, _, _) = recover(&path).unwrap();
        times.push(t.elapsed());
        assert_eq!(rec.frames, frames);
        assert_eq!(rec.events, frames * per as u64);
    }
    report(&format!("verify {} frames x {per} events", frames), &mut times);
    println!("  => per-frame verify: {:?}", times[0] / frames as u32);
}

// ---------- torn tail ----------

fn bench_torn() {
    let path = tmp("torn.jsonl.zst");
    let mut w = LogWriter::open(&path, "demo").unwrap();
    let per = 100usize;
    for _ in 0..1000 {
        let batch: Vec<String> = (0..per).map(|_| event(w.seq())).collect();
        w.append_frame(&batch, Profile::Fast).unwrap();
    }
    // truncate inside the final frame only (last_start + 100 bytes)
    let (boundaries, _) = scan_frames(&path).unwrap();
    let (last_start, last_len) = *boundaries.last().unwrap();
    let cut = last_start + 100;
    let len = std::fs::metadata(&path).unwrap().len();
    let f = File::options().write(true).open(&path).unwrap();
    f.set_len(cut).unwrap();
    drop(f);
    let (rec, offset, _) = recover(&path).unwrap();
    println!("torn: file {len} -> truncated to {offset} (cut at {cut}); recovered frames={} events={} last_seq={} truncated={}",
        rec.frames, rec.events, rec.last_seq, rec.truncated);
    assert!(rec.truncated);
    assert_eq!(rec.frames, 999, "all complete frames must survive; last frame len {last_len}");
    assert_eq!(rec.events, 99_900);
    let _ = std::fs::remove_file(&path);
}

// ---------- mid-file corruption ----------

fn bench_corrupt() {
    let path = tmp("corrupt.jsonl.zst");
    let mut w = LogWriter::open(&path, "demo").unwrap();
    let per = 100usize;
    for _ in 0..500 {
        let batch: Vec<String> = (0..per).map(|_| event(w.seq())).collect();
        w.append_frame(&batch, Profile::Fast).unwrap();
    }
    // flip a byte in frame 250: find its offset via the same boundary scan
    let (boundaries, _) = scan_frames(&path).unwrap();
    let (target_start, target_len) = boundaries[250];
    let target = target_start + 10;
    let mut file = File::options().read(true).write(true).open(&path).unwrap();
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(target)).unwrap();
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(std::io::SeekFrom::Start(target)).unwrap();
    file.write_all(&[byte[0] ^ 0xff]).unwrap();
    drop(file);
    match recover(&path) {
        Err(e) => println!("corrupt: detected as expected (frame at {target_start}+{target_len}): {e}"),
        Ok(_) => panic!("corruption NOT detected"),
    }
}

// ---------- kill -9 durability drill ----------

fn writer_child(path: &str, frames: u64, per: usize) {
    let mut w = LogWriter::open(Path::new(path), "demo").unwrap();
    let mut out = std::io::stdout();
    for _ in 0..frames {
        let batch: Vec<String> = (0..per).map(|_| event(w.seq())).collect();
        let plan: kb_s3_appendlog::FramePlan = w.append_frame(&batch, Profile::Strict).unwrap();
        // ack after the frame is fsynced (strict profile)
        writeln!(out, "ack {}", plan.last_seq).unwrap();
        out.flush().unwrap();
    }
}

fn bench_kill9(frames: u64, per: usize) {
    let path = tmp("kill9.jsonl.zst");
    let exe = std::env::current_exe().unwrap();
    let mut child: Child = Command::new(exe)
        .arg("writer-child").arg(path.to_str().unwrap()).arg(frames.to_string()).arg(per.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut acks = 0u64;
    let mut last_acked = 0u64;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    // read acks for a bounded time so the child is mid-write when we kill
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        match lines.next() {
            Some(Ok(l)) if l.starts_with("ack ") => {
                acks += 1;
                last_acked = l.trim_start_matches("ack ").parse().unwrap();
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    // drain pipe data that was in flight at kill time
    for l in lines {
        if let Ok(l) = l {
            if l.starts_with("ack ") {
                acks += 1;
                last_acked = l.trim_start_matches("ack ").parse().unwrap();
            }
        }
    }
    let (rec, offset, _) = recover(Path::new(&path)).unwrap();
    println!("kill9: acked={acks} (last {last_acked}); recovered frames={} events={} last_seq={} truncated={}; file ends at {offset}",
        rec.frames, rec.events, rec.last_seq, rec.truncated);
    // strict profile: every acked event survives; at most one extra frame may
    // be durable whose ack was still in the pipe at kill time
    assert!(rec.events >= acks * per as u64, "every acked event must survive kill -9");
    assert!(rec.events <= (acks + 1) * per as u64, "at most one unacked frame durable");
    assert!(rec.last_seq >= last_acked);
}

// ---------- dirsync cost (object install protocol) ----------

fn bench_dirsync(n: usize) {
    let dir = std::env::temp_dir().join("kb-s3-dirsync");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut times = Vec::new();
    for i in 0..n {
        let src = dir.join("tmp");
        let dst = dir.join(format!("obj-{i}"));
        let mut f = File::create(&src).unwrap();
        f.write_all(b"payload").unwrap();
        f.sync_all().unwrap();
        drop(f);
        let t = Instant::now();
        std::fs::rename(&src, &dst).unwrap();
        let d = File::open(&dir).unwrap();
        d.sync_all().unwrap();
        drop(d);
        times.push(t.elapsed());
    }
    report(&format!("dirsync+rename n={n}"), &mut times);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- fsync off the critical path ----------

enum Job {
    Frame,
    Flush { done: Sender<()> },
}

struct AsyncLog {
    file: Arc<File>,
    jobs: Sender<Job>,
    frames: u64,
}

impl AsyncLog {
    fn open(path: &Path) -> Self {
        let file = Arc::new(File::options().append(true).create(true).open(path).unwrap());
        let (jobs, rx) = unbounded::<Job>();
        let f = file.clone();
        std::thread::spawn(move || {
            // batch: sync once after draining whatever is queued
            loop {
                match rx.recv() {
                    Ok(Job::Flush { done }) => {
                        f.sync_all().unwrap();
                        done.send(()).ok();
                    }
                    Ok(Job::Frame) => {
                        let mut pending = 1u64;
                        while let Ok(j) = rx.try_recv() {
                            match j {
                                Job::Frame => pending += 1,
                                Job::Flush { done } => {
                                    f.sync_all().unwrap();
                                    done.send(()).ok();
                                    pending = 0;
                                }
                            }
                        }
                        if pending > 0 {
                            f.sync_all().unwrap();
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self { file, jobs, frames: 0 }
    }

    /// Append one frame; ACK immediately after write, fsync happens on the
    /// background thread (bounded interval via batching).
    fn append(&mut self, frame: &[u8]) {
        self.file.write_all(frame).unwrap();
        self.frames += 1;
        self.jobs.send(Job::Frame).ok();
    }

    /// Wait for a full fsync (effect-dispatch / terminal-fact path).
    fn flush(&self) {
        let (done, rx) = bounded(1);
        self.jobs.send(Job::Flush { done }).ok();
        rx.recv().ok();
    }
}

fn bench_asyncfsync(rate: u64, secs: u64) {
    let path = tmp("asyncfsync.jsonl.zst");
    let mut log = AsyncLog::open(&path);
    let interval = Duration::from_micros(1_000_000 / rate);
    let start = Instant::now();
    let mut times = Vec::new();
    let mut n = 0u64;
    let mut flushes = Vec::new();
    while start.elapsed() < Duration::from_secs(secs) {
        let t = Instant::now();
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.include_checksum(true).unwrap();
        let canonical = format!("{{\"stream\":\"demo\",\"schema\":1,\"first_seq\":{n},\"last_seq\":{n},\"count\":1,\"prev\":\"00\",\"created_us\":0}}\n{{\"seq\":{n},\"kind\":\"user_message\"}}\n");
        enc.set_pledged_src_size(Some(canonical.len() as u64)).unwrap();
        enc.write_all(canonical.as_bytes()).unwrap();
        let frame = enc.finish().unwrap();
        log.append(&frame);
        times.push(t.elapsed());
        n += 1;
        if n % 500 == 0 {
            let t = Instant::now();
            log.flush();
            flushes.push(t.elapsed());
        }
        std::thread::sleep(interval);
    }
    log.flush();
    report(&format!("asyncfsync commit ACK (rate {rate}/s, {} events)", n), &mut times);
    report(&format!("asyncfsync flush (effect-dispatch wait)"), &mut flushes);
}

// ---------- main ----------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()).unwrap_or("") {
        "sweep" => bench_sweep(),
        "verify" => {
            let frames: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            bench_verify(frames);
        }
        "torn" => bench_torn(),
        "corrupt" => bench_corrupt(),
        "writer-child" => {
            writer_child(&args[2], args[3].parse().unwrap(), args[4].parse().unwrap());
        }
        "kill9" => {
            let frames: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            let per: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
            bench_kill9(frames, per);
        }
        "dirsync" => {
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
            bench_dirsync(n);
        }
        "asyncfsync" => {
            let rate: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
            let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            bench_asyncfsync(rate, secs);
        }
        "export" => {
            let n = export(Path::new(&args[2])).unwrap();
            println!("exported {n} events");
        }
        _ => {
            eprintln!("usage: kb-s3-appendlog <sweep|verify [n]|torn|corrupt|kill9 [frames] [per]|dirsync [n]|asyncfsync [rate] [secs]|export <path>>");
        }
    }
}
