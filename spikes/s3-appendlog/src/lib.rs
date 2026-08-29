//! S3 spike: AppendLog framing, per R-23 — one typed metadata record + event
//! records per zstd frame, local hash chain, torn-tail recovery, zstdcat
//! equivalence, durability drills, and fsync-off-critical-path.
//! Disposable spike code — never promoted into the implementation.
//!
//! Frame layout (format-freeze input):
//!   zstd frame == metadata JSONL line + event JSONL lines
//!   metadata = {stream, schema, first_seq, last_seq, count, prev, digest, created_us}
//!   digest = blake3 over canonical bytes (metadata minus the digest field, +
//!            event lines); prev = digest of the previous frame (zeros for the
//!            first frame); the zstd frame header carries the pledged content
//!            size, giving O(1) frame boundaries for recovery and exact
//!            torn-tail truncation without any sidecar index.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;

pub fn new_prev() -> [u8; 32] {
    [0u8; 32]
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64
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

#[derive(Clone, Copy, PartialEq)]
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
    pub fn name(self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Balanced => "balanced",
            Profile::Strict => "strict",
        }
    }
}

pub struct FramePlan {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
}

pub struct LogWriter {
    file: File,
    stream: String,
    seq: u64,
    prev: [u8; 32],
    frames: u64,
    pub bytes_written: u64,
}

impl LogWriter {
    pub fn open(path: &Path, stream: &str) -> std::io::Result<Self> {
        let file = File::options().append(true).create(true).open(path)?;
        Ok(Self { file, stream: stream.to_string(), seq: 0, prev: new_prev(), frames: 0, bytes_written: 0 })
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one frame containing `events` (never split across frames).
    // clippy 1.98 warns on `% 10 == 0`; this spike predates the lint and is
    // frozen — suppress rather than rewrite
    #[allow(clippy::manual_is_multiple_of)]
    pub fn append_frame(&mut self, events: &[String], profile: Profile) -> std::io::Result<FramePlan> {
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
        let canonical = canonical(&meta_without_digest(&meta), events);
        meta.digest = hex(blake3::hash(&canonical).as_bytes());
        let mut meta_json = serde_json::to_vec(&meta).unwrap();
        meta_json.push(b'\n');

        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
        enc.include_checksum(true)?;
        enc.set_pledged_src_size(Some((meta_json.len() + events.iter().map(|e| e.len() + 1).sum::<usize>()) as u64))?;
        enc.write_all(&meta_json)?;
        for e in events {
            enc.write_all(e.as_bytes())?;
            enc.write_all(b"\n")?;
        }
        let frame = enc.finish()?;
        let frame_len = frame.len() as u64;

        self.file.write_all(&frame)?;
        self.bytes_written += frame_len;
        self.frames += 1;
        self.prev = *blake3::hash(&canonical).as_bytes();
        match profile {
            Profile::Fast => {}
            Profile::Strict => self.file.sync_all()?,
            Profile::Balanced => {
                if self.frames % 10 == 0 {
                    self.file.sync_all()?;
                }
            }
        }
        Ok(FramePlan { first_seq: first, last_seq: last, count: events.len() as u64, frame_len })
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }
}

// ---------- reader / recovery ----------

#[derive(Debug)]
pub enum RecoveryError {
    Corruption { frame: u64, at: u64, reason: String },
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoveryError::Corruption { frame, at, reason } => write!(f, "corruption at frame {frame} offset {at}: {reason}"),
        }
    }
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

/// zstd frame magic (little-endian).
pub const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Scan frame boundaries. Returns (start, len) per frame and whether the file
/// ended with a torn tail. Magic mismatch anywhere = corruption.
pub fn scan_frames(path: &Path) -> Result<(Vec<(u64, u64)>, bool), RecoveryError> {
    let mut file = File::open(path).map_err(|e| RecoveryError::Corruption { frame: 0, at: 0, reason: format!("open: {e}") })?;
    let file_len = file.metadata().map_err(|e| RecoveryError::Corruption { frame: 0, at: 0, reason: format!("meta: {e}") })?.len();
    let mut out = Vec::new();
    let mut offset: u64 = 0;
    let mut truncated = false;
    loop {
        let mut magic = [0u8; 4];
        let got = read_at(&mut file, offset, &mut magic).map_err(|e| RecoveryError::Corruption { frame: out.len() as u64, at: offset, reason: format!("read: {e}") })?;
        if got == 0 {
            break;
        }
        if magic != MAGIC {
            return Err(RecoveryError::Corruption { frame: out.len() as u64, at: offset, reason: "frame magic mismatch".into() });
        }
        // find the frame's compressed length; grow the probe until it resolves
        let mut buf = vec![0u8; 4096];
        let mut len: Option<u64> = None;
        loop {
            let got = read_at(&mut file, offset, &mut buf).map_err(|e| RecoveryError::Corruption { frame: out.len() as u64, at: offset, reason: format!("read: {e}") })?;
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
        let _ = file_len;
    }
    Ok((out, truncated))
}

/// Read all frames, verify the chain and digests, recover a torn tail
/// (truncated final frame is discarded; mid-file corruption is an explicit
/// error). Returns stats and the last good file offset.
pub fn recover(path: &Path) -> Result<(Recovered, u64, Vec<FrameInfo>), RecoveryError> {
    let (boundaries, truncated) = scan_frames(path)?;
    let mut file = File::open(path).map_err(|e| RecoveryError::Corruption { frame: 0, at: 0, reason: format!("open: {e}") })?;
    let mut frames: Vec<FrameInfo> = Vec::new();
    let mut prev: [u8; 32] = new_prev();
    for (frame_idx, (start, len)) in boundaries.iter().enumerate() {
        let mut buf = vec![0u8; *len as usize];
        let got = read_at(&mut file, *start, &mut buf).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("read: {e}") })?;
        if got as u64 != *len {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: "short read".into() });
        }
        let decoded = zstd::stream::decode_all(&buf[..]).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("zstd: {e}") })?;
        let text = String::from_utf8(decoded).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("utf8: {e}") })?;
        let mut lines = text.lines();
        let meta_line = lines.next().unwrap_or_default();
        let events: Vec<String> = lines.map(|l| l.to_string()).collect();
        let meta: Meta = serde_json::from_str(meta_line).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("meta: {e}") })?;
        if meta.schema != SCHEMA {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("schema {} != {SCHEMA}", meta.schema) });
        }
        let canonical = canonical(&meta_without_digest(&meta), &events);
        let got_digest = hex(blake3::hash(&canonical).as_bytes());
        if got_digest != meta.digest {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("digest {got_digest} != {}", meta.digest) });
        }
        if meta.prev != hex(&prev) {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("chain: prev {} != {}", meta.prev, hex(&prev)) });
        }
        if meta.count as usize != events.len() {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: "count mismatch".into() });
        }
        if meta.first_seq != offset_seq(&frames, meta.count) {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("seq gap: first {} != {}", meta.first_seq, offset_seq(&frames, meta.count)) });
        }
        prev = *blake3::hash(&canonical).as_bytes();
        frames.push(FrameInfo { meta, events });
    }

    let offset = boundaries.last().map(|(s, l)| s + l).unwrap_or(0);
    if truncated {
        let f = File::options().write(true).open(path)
            .map_err(|e| RecoveryError::Corruption { frame: frames.len() as u64, at: offset, reason: format!("truncate open: {e}") })?;
        f.set_len(offset).map_err(|e| RecoveryError::Corruption { frame: frames.len() as u64, at: offset, reason: format!("truncate: {e}") })?;
    }

    let events = frames.iter().map(|f| f.events.len() as u64).sum::<u64>();
    let last_seq = frames.last().map(|f| f.meta.last_seq).unwrap_or(0);
    Ok((Recovered { events, frames: frames.len() as u64, truncated, last_seq }, offset, frames))
}

fn offset_seq(frames: &[FrameInfo], _count: u64) -> u64 {
    frames.last().map(|f| f.meta.last_seq + 1).unwrap_or(0)
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset))?;
    file.read(buf)
}

/// zstdcat-equivalent: plain JSONL events, no frame metadata.
pub fn export(path: &Path) -> std::io::Result<u64> {
    let (rec, _, frames) = recover(path).map_err(|e| std::io::Error::other(format!("recover: {e}")))?;
    let mut out = std::io::BufWriter::new(std::io::stdout());
    let mut n = 0u64;
    for f in frames {
        for e in f.events {
            writeln!(out, "{e}")?;
            n += 1;
        }
    }
    out.flush()?;
    assert_eq!(n, rec.events);
    Ok(n)
}

/// Streaming variant: yields one frame at a time, keeping only one frame's
/// events in memory (rebuild path — S5). Verifies the chain and digests.
pub fn for_each_frame<F>(path: &Path, mut f: F) -> Result<Recovered, RecoveryError>
where
    F: FnMut(FrameInfo) -> std::io::Result<()>,
{
    let (boundaries, truncated) = scan_frames(path)?;
    let mut file = File::open(path).map_err(|e| RecoveryError::Corruption { frame: 0, at: 0, reason: format!("open: {e}") })?;
    let mut prev: [u8; 32] = new_prev();
    let mut events = 0u64;
    let mut last_seq = 0u64;
    for (frame_idx, (start, len)) in boundaries.iter().enumerate() {
        let mut buf = vec![0u8; *len as usize];
        let got = read_at(&mut file, *start, &mut buf).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("read: {e}") })?;
        if got as u64 != *len {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: "short read".into() });
        }
        let decoded = zstd::stream::decode_all(&buf[..]).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("zstd: {e}") })?;
        let text = String::from_utf8(decoded).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("utf8: {e}") })?;
        let mut lines = text.lines();
        let meta_line = lines.next().unwrap_or_default();
        let frame_events: Vec<String> = lines.map(|l| l.to_string()).collect();
        let meta: Meta = serde_json::from_str(meta_line).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("meta: {e}") })?;
        if meta.schema != SCHEMA {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("schema {} != {SCHEMA}", meta.schema) });
        }
        let canonical = canonical(&meta_without_digest(&meta), &frame_events);
        let got_digest = hex(blake3::hash(&canonical).as_bytes());
        if got_digest != meta.digest {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("digest {got_digest} != {}", meta.digest) });
        }
        if meta.prev != hex(&prev) {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("chain: prev {} != {}", meta.prev, hex(&prev)) });
        }
        if meta.count as usize != frame_events.len() {
            return Err(RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: "count mismatch".into() });
        }
        prev = *blake3::hash(&canonical).as_bytes();
        f(FrameInfo { meta, events: frame_events }).map_err(|e| RecoveryError::Corruption { frame: frame_idx as u64, at: *start, reason: format!("consumer: {e}") })?;
        events += 1;
        last_seq = *start;
    }
    Ok(Recovered { events, frames: boundaries.len() as u64, truncated, last_seq })
}
