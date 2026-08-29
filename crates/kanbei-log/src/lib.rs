//! AppendLog: the frozen frame format (ratification packet §2) and the
//! durability-queue commit path (§3). Format source: `spikes/s3-appendlog`,
//! byte-for-byte frozen.
//!
//! Frame layout: one zstd frame per commit; the first record is a typed
//! metadata JSONL line, then event JSONL records; events are never split
//! across frames. Metadata = `{stream, schema, first_seq, last_seq, count,
//! prev, digest, created_us}`; `digest` = blake3 over canonical bytes
//! (metadata minus the digest field, then the event lines); `prev` = digest
//! of the previous frame (zeros for genesis). zstd level 3, content checksum
//! ON, pledged content size ON — the pledged size gives O(1) frame boundaries
//! and exact torn-tail truncation without a sidecar index.
//!
//! Commit path (§3): write + enqueue [`SyncOp::Fsync`] on the shared
//! [`DurabilityQueue`]; the queue's thread executes fsyncs strictly FIFO.
//! [`Profile`] picks the flush cadence: Fast and Balanced ack after
//! write+enqueue (the caller flushes on its own cadence), Strict flushes
//! before returning, so an ack implies durable.
//!
//! [`recover`] verifies the chain and **truncates** a torn final frame to the
//! last good offset — the file is modified. [`AppendLog::open`] does NOT
//! recover: run [`recover`] before reopening a log that may have a torn tail,
//! or frames appended after the tear are lost on the next recovery.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use kanbei_core::envelope::Envelope;
use kanbei_core::queue::{DurabilityQueue, SyncOp};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA: u32 = 1;

/// zstd frame magic (little-endian).
pub const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Genesis chain anchor: the "previous" frame's digest before frame 1.
pub fn new_prev() -> [u8; 32] {
    [0u8; 32]
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

// ---------- frame metadata ----------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Meta {
    pub stream: String,
    pub schema: u32,
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub prev: String,
    pub digest: String,
    pub created_us: u64,
}

/// The bytes the frame digest covers: metadata with the digest field dropped,
/// then the event lines.
fn canonical(meta_no_digest: &serde_json::Value, events: &[String]) -> Vec<u8> {
    let mut out = serde_json::to_vec(meta_no_digest).unwrap();
    out.push(b'\n');
    for e in events {
        out.extend_from_slice(e.as_bytes());
        out.push(b'\n');
    }
    out
}

fn meta_without_digest(m: &Meta) -> serde_json::Value {
    serde_json::json!({
        "stream": m.stream,
        "schema": m.schema,
        "first_seq": m.first_seq,
        "last_seq": m.last_seq,
        "count": m.count,
        "prev": m.prev,
        "created_us": m.created_us,
    })
}

// ---------- writer ----------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Profile {
    Fast,
    Balanced,
    Strict,
}

impl Profile {
    pub fn from(s: &str) -> Self {
        match s {
            "fast" => Profile::Fast,
            "strict" => Profile::Strict,
            _ => Profile::Balanced,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Balanced => "balanced",
            Profile::Strict => "strict",
        }
    }
}

#[derive(Debug)]
pub struct FramePlan {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
}

pub struct AppendLog {
    file: File,
    stream: String,
    seq: u64,
    prev: [u8; 32],
    frames: u64,
    queue: Arc<DurabilityQueue>,
}

impl AppendLog {
    /// Append-mode open. Does NOT recover an existing file: the caller runs
    /// [`recover`] first (recovery truncates a torn tail). The next seq and
    /// chain anchor continue from the last complete frame's metadata; a
    /// fresh/empty log starts at seq 1 with a zeros prev (genesis).
    pub fn open(path: &Path, stream: &str, queue: Arc<DurabilityQueue>) -> io::Result<Self> {
        let file = File::options().append(true).create(true).open(path)?;
        let (seq, prev) = tail_state(path)?;
        Ok(Self { file, stream: stream.to_string(), seq, prev, frames: 0, queue })
    }

    /// Next expected event seq.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one frame containing `events` (never split across frames).
    /// Every envelope is validated and must continue the seq chain
    /// (`events[i].seq == seq + i`); the frame is written, then an fsync for
    /// it is enqueued on the durability queue. Strict additionally flushes
    /// before returning, so its ack implies durable.
    pub fn append(&mut self, events: &[Envelope], profile: Profile) -> io::Result<FramePlan> {
        if events.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "append: empty event batch"));
        }
        for (i, e) in events.iter().enumerate() {
            e.validate().map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("append: envelope {i} (seq {}): {err}", e.seq),
                )
            })?;
            let expected = self.seq + i as u64;
            if e.seq != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("append: envelope {i} seq {} != expected {expected}", e.seq),
                ));
            }
        }
        let lines: Vec<String> = events.iter().map(Envelope::to_line).collect();
        let first = self.seq;
        self.seq += events.len() as u64;
        let last = self.seq - 1;
        let mut meta = Meta {
            stream: self.stream.clone(),
            schema: SCHEMA,
            first_seq: first,
            last_seq: last,
            count: events.len() as u64,
            prev: hex(&self.prev),
            digest: String::new(),
            created_us: now_us(),
        };
        let canonical = canonical(&meta_without_digest(&meta), &lines);
        let digest = blake3::hash(&canonical);
        meta.digest = hex(digest.as_bytes());
        let mut meta_json = serde_json::to_vec(&meta).unwrap();
        meta_json.push(b'\n');

        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
        enc.include_checksum(true)?;
        enc.set_pledged_src_size(Some(
            (meta_json.len() + lines.iter().map(|l| l.len() + 1).sum::<usize>()) as u64,
        ))?;
        enc.write_all(&meta_json)?;
        for l in &lines {
            enc.write_all(l.as_bytes())?;
            enc.write_all(b"\n")?;
        }
        let frame = enc.finish()?;
        let frame_len = frame.len() as u64;

        self.file.write_all(&frame)?;
        self.frames += 1;
        self.prev = *digest.as_bytes();
        // try_clone hands the queue a fresh fd for the same inode — the queue's
        // thread may hold it while this writer keeps appending.
        self.queue.enqueue(SyncOp::Fsync(self.file.try_clone()?))?;
        match profile {
            Profile::Fast | Profile::Balanced => {}
            Profile::Strict => self.flush()?,
        }
        Ok(FramePlan { first_seq: first, last_seq: last, count: events.len() as u64, frame_len })
    }

    /// Barrier: wait until every queued durability op has run.
    pub fn flush(&self) -> io::Result<()> {
        self.queue.flush()
    }

    /// Frames appended by this writer since open.
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

/// Next seq and chain anchor derived from the last complete frame (no
/// verification — that is [`recover`]'s job).
fn tail_state(path: &Path) -> io::Result<(u64, [u8; 32])> {
    let (boundaries, _) = scan_frames(path).map_err(|e| io::Error::other(format!("open: {e}")))?;
    let Some(&(start, len)) = boundaries.last() else { return Ok((1, new_prev())); };
    let mut file = File::open(path)?;
    let (meta, events) =
        read_frame(&mut file, start, len, 0).map_err(|e| io::Error::other(format!("open: {e}")))?;
    let canonical = canonical(&meta_without_digest(&meta), &events);
    Ok((meta.last_seq + 1, *blake3::hash(&canonical).as_bytes()))
}

// ---------- reader / recovery ----------

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("corruption at frame {frame} offset {offset}: {reason}")]
    Corruption { frame: u64, offset: u64, reason: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct FrameInfo {
    pub meta: Meta,
    pub events: Vec<String>,
}

pub struct Recovered {
    pub events: u64,
    pub frames: u64,
    pub truncated: bool,
    pub last_seq: u64,
}

/// Scan frame boundaries. Returns (start, len) per frame and whether the file
/// ended with a torn tail. Magic mismatch anywhere = corruption.
pub fn scan_frames(path: &Path) -> Result<(Vec<(u64, u64)>, bool), RecoveryError> {
    let mut file = File::open(path)?;
    let mut out = Vec::new();
    let mut offset: u64 = 0;
    let mut truncated = false;
    loop {
        let mut magic = [0u8; 4];
        let got = read_at(&mut file, offset, &mut magic)?;
        if got == 0 {
            break;
        }
        if magic != MAGIC {
            return Err(RecoveryError::Corruption {
                frame: out.len() as u64,
                offset,
                reason: "frame magic mismatch".into(),
            });
        }
        // find the frame's compressed length; grow the probe until it resolves
        let mut buf = vec![0u8; 4096];
        let mut len: Option<u64> = None;
        loop {
            let got = read_at(&mut file, offset, &mut buf)?;
            match zstd::zstd_safe::find_frame_compressed_size(&buf[..got]) {
                Ok(n) if n > 0 => {
                    len = Some(n as u64);
                    break;
                }
                _ => {
                    if got < buf.len() {
                        // read less than asked: EOF reached with the frame incomplete
                        truncated = true;
                        break;
                    }
                    buf.resize(buf.len() * 4, 0);
                }
            }
        }
        let Some(len) = len else { break };
        out.push((offset, len));
        offset += len;
    }
    Ok((out, truncated))
}

/// Read one frame's raw records (metadata line + event lines).
fn read_frame(
    file: &mut File,
    start: u64,
    len: u64,
    frame: u64,
) -> Result<(Meta, Vec<String>), RecoveryError> {
    let mut buf = vec![0u8; len as usize];
    let got = read_at(file, start, &mut buf)?;
    if got as u64 != len {
        return Err(RecoveryError::Corruption { frame, offset: start, reason: "short read".into() });
    }
    let decoded = zstd::stream::decode_all(&buf[..]).map_err(|e| {
        RecoveryError::Corruption { frame, offset: start, reason: format!("zstd: {e}") }
    })?;
    let text = String::from_utf8(decoded).map_err(|e| {
        RecoveryError::Corruption { frame, offset: start, reason: format!("utf8: {e}") }
    })?;
    let mut lines = text.lines();
    let meta_line = lines.next().unwrap_or_default();
    let events: Vec<String> = lines.map(|l| l.to_string()).collect();
    let meta: Meta = serde_json::from_str(meta_line).map_err(|e| {
        RecoveryError::Corruption { frame, offset: start, reason: format!("meta: {e}") }
    })?;
    Ok((meta, events))
}

/// Verify schema, digest, prev chain, count, and seq continuity for one
/// frame; returns the frame digest that chains into the next frame.
fn verify_frame(
    meta: &Meta,
    events: &[String],
    prev: &[u8; 32],
    expected_first: u64,
    frame: u64,
    offset: u64,
) -> Result<[u8; 32], RecoveryError> {
    if meta.schema != SCHEMA {
        return Err(RecoveryError::Corruption {
            frame,
            offset,
            reason: format!("schema {} != {SCHEMA}", meta.schema),
        });
    }
    let canonical = canonical(&meta_without_digest(meta), events);
    let got_digest = hex(blake3::hash(&canonical).as_bytes());
    if got_digest != meta.digest {
        return Err(RecoveryError::Corruption {
            frame,
            offset,
            reason: format!("digest {got_digest} != {}", meta.digest),
        });
    }
    if meta.prev != hex(prev) {
        return Err(RecoveryError::Corruption {
            frame,
            offset,
            reason: format!("chain: prev {} != {}", meta.prev, hex(prev)),
        });
    }
    if meta.count as usize != events.len() {
        return Err(RecoveryError::Corruption { frame, offset, reason: "count mismatch".into() });
    }
    if meta.first_seq != expected_first {
        return Err(RecoveryError::Corruption {
            frame,
            offset,
            reason: format!("seq gap: first {} != {expected_first}", meta.first_seq),
        });
    }
    if let Err(reason) = check_event_seqs(meta, events) {
        return Err(RecoveryError::Corruption { frame, offset, reason: format!("seq: {reason}") });
    }
    Ok(*blake3::hash(&canonical).as_bytes())
}

/// Each event line must be JSON whose `seq` continues the frame's range.
fn check_event_seqs(meta: &Meta, events: &[String]) -> Result<(), String> {
    for (i, line) in events.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("event {i}: not json: {e}"))?;
        let seq = v
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("event {i}: no numeric seq"))?;
        let expected = meta.first_seq + i as u64;
        if seq != expected {
            return Err(format!("event {i}: seq {seq} != {expected}"));
        }
    }
    Ok(())
}

fn offset_seq(frames: &[FrameInfo]) -> u64 {
    // genesis first frame starts at seq 1
    frames.last().map(|f| f.meta.last_seq + 1).unwrap_or(1)
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    use std::io::Seek;
    file.seek(io::SeekFrom::Start(offset))?;
    file.read(buf)
}

/// Read all frames, verify the chain and digests, and recover a torn tail:
/// an incomplete final frame is TRUNCATED from the file (set_len to the last
/// good offset) and reported via `truncated`. Mid-file corruption is an
/// explicit error and leaves the file untouched.
pub fn recover(path: &Path) -> Result<Recovered, RecoveryError> {
    let (boundaries, truncated) = scan_frames(path)?;
    let mut file = File::open(path)?;
    let mut prev = new_prev();
    let mut frames: Vec<FrameInfo> = Vec::new();
    for (frame_idx, (start, len)) in boundaries.iter().enumerate() {
        let (meta, events) = read_frame(&mut file, *start, *len, frame_idx as u64)?;
        prev = verify_frame(&meta, &events, &prev, offset_seq(&frames), frame_idx as u64, *start)?;
        frames.push(FrameInfo { meta, events });
    }

    let offset = boundaries.last().map(|(s, l)| s + l).unwrap_or(0);
    if truncated {
        let f = File::options().write(true).open(path)?;
        f.set_len(offset)?;
    }

    let events = frames.iter().map(|f| f.events.len() as u64).sum::<u64>();
    let last_seq = frames.last().map(|f| f.meta.last_seq).unwrap_or(0);
    Ok(Recovered { events, frames: frames.len() as u64, truncated, last_seq })
}

/// Streaming variant: yields one frame at a time, keeping only one frame's
/// events in memory (rebuild path — S5). Verifies the chain and digests but
/// never truncates.
pub fn for_each_frame(path: &Path, mut f: impl FnMut(&FrameInfo)) -> Result<Recovered, RecoveryError> {
    let (boundaries, truncated) = scan_frames(path)?;
    let mut file = File::open(path)?;
    let mut prev = new_prev();
    let mut events = 0u64;
    let mut last_seq = 0u64;
    for (frame_idx, (start, len)) in boundaries.iter().enumerate() {
        let (meta, frame_events) = read_frame(&mut file, *start, *len, frame_idx as u64)?;
        prev = verify_frame(&meta, &frame_events, &prev, last_seq + 1, frame_idx as u64, *start)?;
        let info = FrameInfo { meta, events: frame_events };
        f(&info);
        events += info.events.len() as u64;
        last_seq = info.meta.last_seq;
    }
    Ok(Recovered { events, frames: boundaries.len() as u64, truncated, last_seq })
}

/// zstdcat-equivalent: plain JSONL events, no frame metadata. Returns the
/// event count. Read-only: unlike [`recover`], a torn tail is not truncated.
pub fn export(path: &Path) -> io::Result<u64> {
    let mut out = io::BufWriter::new(io::stdout());
    let n = export_to(path, &mut out)?;
    out.flush()?;
    Ok(n)
}

fn export_to<W: Write>(path: &Path, out: &mut W) -> io::Result<u64> {
    let mut n = 0u64;
    let mut first_err: Option<io::Error> = None;
    let rec = for_each_frame(path, |info| {
        for e in &info.events {
            if first_err.is_some() {
                return;
            }
            match writeln!(out, "{e}") {
                Ok(()) => n += 1,
                Err(e) => {
                    first_err = Some(e);
                    return;
                }
            }
        }
    })
    .map_err(|e| io::Error::other(format!("recover: {e}")))?;
    if let Some(e) = first_err {
        return Err(e);
    }
    debug_assert_eq!(n, rec.events);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(seq: u64) -> Envelope {
        Envelope {
            env: kanbei_core::envelope::ENVELOPE_SCHEMA,
            seq,
            evt: format!("evt{seq}"),
            kind: "user_message".into(),
            payload_schema: 1,
            payload: json!({"text": format!("hello {seq}")}),
            refs: vec![],
            snapshot: None,
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kb-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn export_emits_plain_jsonl_events() {
        let path = tmp("export");
        let queue = Arc::new(DurabilityQueue::start("kb-log-test-export"));
        let mut log = AppendLog::open(&path, "demo", Arc::clone(&queue)).unwrap();
        let batch: Vec<Envelope> = (1..=20).map(env).collect();
        log.append(&batch, Profile::Fast).unwrap();
        drop(log);

        let mut out = Vec::new();
        let n = export_to(&path, &mut out).unwrap();
        assert_eq!(n, 20);
        let expected: String = batch.iter().map(|e| format!("{}\n", e.to_line())).collect();
        assert_eq!(String::from_utf8(out).unwrap(), expected);

        match Arc::try_unwrap(queue) {
            Ok(q) => q.shutdown().unwrap(),
            Err(_) => panic!("durability queue still referenced at test shutdown"),
        }
    }
}
