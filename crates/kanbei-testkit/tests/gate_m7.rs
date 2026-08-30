#![allow(clippy::result_large_err)]

//! The M7 dogfooding gate (docs/dogfooding-instrument.md, ratified
//! pre-M3): the six-task battery against real fixture repos through the
//! real session kernel, plus the interrupted-task SIGKILL matrix, the
//! spend-breaker scenario, and the scaled unattended-hour measurement.
//! Every metric derives from canonical session-log records; the gate
//! asserts the instrument's thresholds (sections 1-3) and the per-task
//! success criteria (section 4).

use std::path::PathBuf;
use std::time::Duration;

use kanbei_testkit::dogfood::{
    evaluate_thresholds, format_report, run_battery,
};
use kanbei_testkit::fixture::{
    CLAMP_FIXED, CSVLIB_REFACTORED, MATHLIB_FIB, python_path,
};

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_root(tag: &str) -> TempRoot {
    let d = std::env::temp_dir().join(format!(
        "kb-gate-m7-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&d).unwrap();
    TempRoot(d)
}

/// Run a command the way the battery does (cleared env + PATH/HOME).
fn run_in(repo: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(args[0])
        .args(&args[1..])
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn file(repo: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(repo.join(name)).unwrap()
}

fn git_log(repo: &std::path::Path) -> Vec<String> {
    let (_, out) = run_in(repo, &["git", "log", "--oneline"]);
    out.lines().map(|l| l.to_string()).collect()
}

fn git_show_stat(repo: &std::path::Path) -> String {
    let (_, out) = run_in(repo, &["git", "show", "--stat", "--format=%s", "HEAD"]);
    out
}

fn unittest(repo: &std::path::Path, module: &str) -> bool {
    run_in(repo, &[&python_path(), "-m", "unittest", module]).0
}

#[test]
fn dogfooding_battery_thresholds_hold() {
    let root = fresh_root("battery");
    let report = run_battery(&root.0, Duration::from_secs(180));
    println!("{}", format_report(&report));
    assert_eq!(report.tasks.len(), 6, "six battery task runs (t1-t4 + t5a/t5b)");
    let verdict = evaluate_thresholds(&report);
    assert!(
        verdict.all(),
        "threshold verdict: t1.1={} t1.2={} t1.3={} t1.4={} t1.5={} t2.1={} t2.2={} t2.3={} t3.1={} t3.2={} t3.3={} t3.4={}",
        verdict.t1_1, verdict.t1_2, verdict.t1_3, verdict.t1_4, verdict.t1_5,
        verdict.t2_1, verdict.t2_2, verdict.t2_3, verdict.t3_1, verdict.t3_2,
        verdict.t3_3, verdict.t3_4,
    );
}

#[test]
fn battery_task_success_criteria() {
    let root = fresh_root("criteria");
    let report = run_battery(&root.0, Duration::from_secs(60));
    println!("{}", format_report(&report));

    let t1 = &report.tasks[0];
    assert_eq!(t1.task, 1);
    assert_eq!(file(&t1.repo, "mathlib.py"), CLAMP_FIXED, "minimal fix");
    assert!(
        git_show_stat(&t1.repo).contains("mathlib.py")
            && !git_show_stat(&t1.repo).contains("test_mathlib.py"),
        "fix commit touches only mathlib.py: {}",
        git_show_stat(&t1.repo)
    );
    assert!(unittest(&t1.repo, "test_mathlib"), "suite green on fix commit");

    let t2 = &report.tasks[1];
    assert_eq!(t2.task, 2);
    assert_eq!(file(&t2.repo, "mathlib.py"), MATHLIB_FIB, "fib matches spec");
    assert!(unittest(&t2.repo, "test_mathlib"), "feature suite green");

    let t3 = &report.tasks[2];
    assert_eq!(t3.task, 3);
    assert_eq!(file(&t3.repo, "csvlib.py"), CSVLIB_REFACTORED);
    let stat = git_show_stat(&t3.repo);
    assert!(
        stat.contains("csvlib.py") && !stat.contains("test_csvlib.py"),
        "refactor diff is csvlib.py only: {stat}"
    );
    assert!(unittest(&t3.repo, "test_csvlib"), "refactor suite green");

    let t4 = &report.tasks[3];
    assert_eq!(t4.task, 4);
    let stat = git_show_stat(&t4.repo);
    assert!(
        stat.contains("investigation.md") && !stat.contains("state.py"),
        "investigation commits only the report: {stat}"
    );
    let inv = file(&t4.repo, "investigation.md");
    assert!(
        inv.contains("non-atomically") && inv.contains("os.replace"),
        "root cause + fix proposal present"
    );
    // No code change required: the failing test must still fail.
    let (ok, _) = run_in(&t4.repo, &[&python_path(), "-m", "unittest", "test_integration"]);
    assert!(!ok, "root cause untouched by the investigation");

    // Task 5: part A + part B.
    let _part_a = &report.tasks[4];
    let part_b = &report.tasks[5];
    let log = git_log(&part_b.repo);
    let msgs: Vec<String> = log
        .iter()
        .map(|l| l.split(' ').skip(1).collect::<Vec<_>>().join(" "))
        .collect();
    assert!(
        msgs.iter().filter(|m| m.as_str() == "add lcm").count() == 1
            && msgs.iter().filter(|m| m.as_str() == "add gcd").count() == 1,
        "part B adds lcm only: {log:?}"
    );
    assert!(
        part_b.facts.memory_query_hits.iter().any(|h| h.contains("gcd")),
        "part B cites part A's memory: {:?}",
        part_b.facts.memory_query_hits
    );
    let transition = part_b.facts.branch_transition_seq.expect("part B has a transition");
    let redone = part_b
        .facts
        .intent_tools
        .iter()
        .filter(|(seq, _, _)| *seq > transition)
        .any(|(_, tool, args)| (tool == "fs.write" || tool == "fs.patch") && args.contains("def gcd"));
    assert!(!redone, "part B never re-implements gcd");
    assert!(
        unittest(&part_b.repo, "test_mathlib"),
        "combined gcd+lcm suite green"
    );

    // Task 6 interrupted runs: recovery validity + no dup effects are
    // already asserted inside the matrix (run.dup_effects); T2.1/T2.3 cover
    // them at the threshold level.
    let interrupted = &report.interrupted;
    assert_eq!(interrupted.len(), 7, "six kill windows + control");
    for r in interrupted {
        assert!(r.recovery_valid, "recovery valid for {}", r.kill);
        if r.kill != "control" {
            assert!(!r.dup_effects, "no duplicated effects for {}", r.kill);
            assert_eq!(
                format!("{:?}", r.resumed),
                "CompletedGoal",
                "resume for {}",
                r.kill
            );
        }
    }
}



