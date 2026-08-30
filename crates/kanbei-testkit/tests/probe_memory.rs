#![allow(clippy::result_large_err)]

//! The M7 memory-usefulness probes (docs/memory-probes.md): runs the full
//! battery against the fake engine and `MemoryRootActor`, prints every raw
//! number per probe, and asserts the M7-tuned verdict.

use std::path::PathBuf;

use kanbei_testkit::probes::{ProbesReport, probes_verdict, run_probes};

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_root() -> TempRoot {
    let d = std::env::temp_dir().join(format!(
        "kb-probe-memory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&d).unwrap();
    TempRoot(d)
}

fn print_report(r: &ProbesReport) {
    println!("--- probe report (raw numbers) ---");
    println!("W1  writing fidelity: proposed={} matched={} precision={:.3}", r.w1_proposed, r.w1_matched, r.w1_precision);
    println!("W2  propose approval: proposed={} approved={} rate={:.3}", r.w2_proposed, r.w2_approved, r.w2_rate);
    println!("R1  recall@5:         hits={}/{} recall={:.3}", r.r1_hits, r.r1_queries, r.r1_recall_at_5);
    println!("R2  supersedes annotation: {r2}", r2 = r.r2_survivor_supersedes_annotation);
    println!("T1  supersede across sessions: old_absent={} annotation_preserved={}", r.t1_old_claim_absent, r.t1_annotation_preserved);
    println!("T2  age distribution: oldest_returned_index={} from_last_10_seeded={}", r.t2_oldest_returned_index, r.t2_recent_returned);
    println!("A1  recurring questions: seeded_with_fragment={}/{} fresh_with_fragment={}", r.a1_seeded_with_fragment, r.a1_total, r.a1_fresh_with_fragment);
    println!("A2  cache outcomes:    hits={} invalidated={} misses={}", r.a2_hits, r.a2_invalidated, r.a2_misses);
    println!("C1  short-circuited reads: with_claim={} without_claim={} claim_returned={}", r.c1_reads_with_claim, r.c1_reads_without_claim, r.c1_claim_returned);
    println!("C2  child query has project claim: {} followup={}", r.c2_child_query_has_project_claim, r.c2_followup_outcome);
    println!("L1  project_context:   runs={} p50={:.2}ms p95={:.2}ms", r.l1_runs, r.l1_p50_ms, r.l1_p95_ms);
    println!("L2  fragment tokens:   {:?} (stable budget 4096, volatile 2048)", r.l2_fragments);
    println!("G1  graph growth:      active={} edges={} retracted={} retraction_flag={}", r.g1_active_claims, r.g1_edges, r.g1_retracted, r.g1_retraction_flag);
    println!("G2  reconcile:         200={:.1}ms 500={:.1}ms 1000={:.1}ms", r.g2_reconcile_200_ms, r.g2_reconcile_500_ms, r.g2_reconcile_1000_ms);
}

#[test]
fn memory_probes_meet_m7_thresholds() {
    let root = fresh_root();
    let report = run_probes(&root.0);
    print_report(&report);
    assert!(
        probes_verdict(&report),
        "probe verdict failed: {report:?}"
    );
}
