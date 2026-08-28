//! S6 spike main: write the fixture, reconstruct, and run an upcast-throughput
//! sanity check. Disposable spike code — never promoted into the implementation.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use kb_s3_appendlog::{LogWriter, Profile};
use kb_s6_upcast::{reconstruct, write_fixture, Envelope, Registry};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::env::temp_dir().join("kb-s6");
    let stream = dir.join("session.jsonl.zst");

    match args.get(1).map(|s| s.as_str()).unwrap_or("all") {
        "fixture" => {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            write_fixture(&stream).unwrap();
            println!("fixture written: {}", stream.display());
        }
        "read" => {
            let objects: HashSet<String> = std::fs::read_dir(dir.join("objects"))
                .unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
            let rep = reconstruct(&stream, &Registry::new(), &objects).unwrap();
            println!("reconstruction report: events={}", rep.events);
            for (kind, s) in &rep.kinds {
                println!("  {kind}: schema={} count={} upcasted={} opaque={} reason={:?}", s.schema, s.count, s.upcasted, s.opaque, s.opaque_reason);
            }
            println!("  missing_objects={:?}", rep.missing_objects);
            println!("  upcast_errors={:?}", rep.upcast_errors);
            assert_eq!(rep.missing_objects, vec!["blake3:deadbeef".to_string()]);
            let um = &rep.kinds["user_message"];
            assert_eq!(um.upcasted, 1);
            let tk = &rep.kinds["tool_result"];
            assert_eq!(tk.upcasted, 2);
            let fk = &rep.kinds["future_kind"];
            assert_eq!(fk.opaque, 1);
        }
        "throughput" => {
            // upcast cost over 100k events
            let n = 100_000u64;
            let mut w = LogWriter::open(&dir.join("big.jsonl.zst"), "demo").unwrap();
            let mut batch = Vec::new();
            for i in 0..n {
                let e = Envelope {
                    env: 1, seq: i, evt: format!("e{i}"), kind: "user_message".into(), payload_schema: 1,
                    payload: json!({"text": format!("msg {i}")}), refs: vec![],
                };
                batch.push(e.to_line());
                if batch.len() >= 64 {
                    w.append_frame(&batch, Profile::Fast).unwrap();
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                w.append_frame(&batch, Profile::Fast).unwrap();
            }
            let reg = Registry::new();
            let objects = HashSet::new();
            let t = Instant::now();
            let rep = reconstruct(&dir.join("big.jsonl.zst"), &reg, &objects).unwrap();
            let elapsed = t.elapsed();
            println!("upcast throughput: {n} events in {elapsed:?} ({:.0} ev/s), upcasted={}", n as f64 / elapsed.as_secs_f64(), rep.kinds["user_message"].upcasted);
            assert_eq!(rep.kinds["user_message"].upcasted, n);
        }
        _ => {
            eprintln!("usage: kb-s6-upcast <fixture|read|throughput>");
        }
    }
}
