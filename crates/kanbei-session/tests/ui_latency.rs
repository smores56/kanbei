//! M8 wave 3 latency gate (architecture.md: multi-module UI composition is
//! gated on a latency budget: interactive input ACK p99 <= 50 ms with >= 1
//! background wake/s; the ratified spike measured 3.4 ms under flood).
//!
//! Two UI mounts are bound; a background thread produces wake events
//! continuously (the session is a single-actor type — not Send — so the
//! wakes are committed by the main thread between measured iterations,
//! exactly like a real terminal driving the actor while background wakes
//! queue; the durability worker flushes them concurrently with the measured
//! input handling). The main thread runs 200 `ui_handle_input` iterations
//! (char + Enter submit → `append_user_message` round-trip through both
//! mounts' reducers and the capability intersection) and asserts the
//! round-trip p99 stays within 50 ms, with the wall time bounded so the
//! gate cannot silently become a long benchmark.

use std::sync::mpsc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kanbei_capabilities::TrustClass;
use kanbei_core::id::Id128;
use kanbei_session::{NewEvent, Session, SessionConfig};

mod common;
use common::{engine, require_guest, ui_module};

fn broker_with_append_grant(session_id: Id128, generation: u64) -> kanbei_capabilities::Broker {
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(kanbei_capabilities::PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![kanbei_capabilities::Capability::new(
                "session".into(),
                vec!["append".into()],
            )],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let policy_version = broker.policy_version();
    let mut grant = kanbei_capabilities::Grant {
        grant_digest: kanbei_core::digest::Digest::new(b"m8-latency"),
        principal: kanbei_capabilities::Principal {
            session: session_id,
            generation,
            run: None,
        },
        module_generation: generation,
        capability: kanbei_capabilities::Capability::new(
            "session".into(),
            vec!["append".into()],
        ),
        scope: kanbei_capabilities::GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("m8 latency gate".into()),
        policy_version,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    broker
}

#[test]
fn multi_mount_input_ack_p99_within_budget_under_background_flood() {
    if !require_guest() {
        return;
    }
    // The granted mount is the first activation = generation 1 (generations
    // are deterministic counters from 1, M2), so the submit round-trip
    // completes.
    let session_id = Id128::generate();
    let broker = broker_with_append_grant(session_id, 1);
    let dir = common::tempdir("latency");
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m8-latency".into(),
        broker,
        session_id: Some(session_id),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    session
        .activate_ui(ui_module("main_ui", "main_comp", "main", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false))
        .unwrap();
    assert_eq!(session.ui().unwrap().mounts.len(), 2, "two mounts bound");
    session.ui_render_frame().unwrap();
    // focus the granted mount's input so the round-trip includes submit
    session.ui_handle_input(b"\t").unwrap();

    // Background wake flood: >= 1 wake/s guaranteed (the producer runs at
    // ~1 kHz; the floor is checked below).
    let (tx, rx) = mpsc::channel::<()>();
    let tx_producer = tx.clone();
    let produced = std::sync::Arc::new(AtomicU64::new(0));
    let produced_handle = std::sync::Arc::clone(&produced);
    let producer = std::thread::spawn(move || {
        while tx_producer.send(()).is_ok() {
            produced_handle.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    const ITERATIONS: usize = 200;
    let mut samples: Vec<Duration> = Vec::with_capacity(ITERATIONS);
    let mut wakes_committed: u64 = 0;
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        // commit every pending background wake (the actor drains its wake
        // queue between input events; the durability worker flushes them
        // concurrently with the measured handling)
        for _ in rx.try_iter() {
            session
                .commit(
                    vec![NewEvent {
                        kind: "wake".into(),
                        payload_schema: 1,
                        payload: serde_json::json!({}),
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )
                .unwrap();
            wakes_committed += 1;
        }
        // measured round-trip: char (fan-out reduce to both mounts) + Enter
        // (fan-out reduce + submit intent → append_user_message + refresh)
        let t0 = Instant::now();
        session.ui_handle_input(b"a").unwrap();
        session.ui_handle_input(b"\n").unwrap();
        samples.push(t0.elapsed());
    }
    drop(tx);
    drop(rx);
    producer.join().unwrap();
    let total = started.elapsed();

    assert!(
        wakes_committed >= 100,
        "background flood ran: {wakes_committed} wakes committed in {total:?} \
         (budget floor: >= 1 wake/s)"
    );
    assert!(
        total < Duration::from_secs(5),
        "gate must stay under 5 s wall time, took {total:?}"
    );

    samples.sort();
    let p99 = samples[(samples.len() as f64 * 0.99) as usize];
    let max = samples[samples.len() - 1];
    println!(
        "multi-mount input ACK: n={} p50={:?} p99={:?} max={:?} wall={total:?} wakes={wakes_committed}",
        samples.len(),
        samples[samples.len() / 2],
        p99,
        max
    );
    assert!(
        p99 <= Duration::from_millis(50),
        "input ACK p99 {p99:?} exceeds the 50 ms budget"
    );
    std::fs::remove_dir_all(&dir).ok();
}
