//! M7 dogfooding fixtures: scratch git repositories with deterministic
//! Python tasks (docs/dogfooding-instrument.md §4). Each battery task gets
//! its own fresh repo; the session's `fs_root` is the repo directory, so the
//! native tools can never escape the fixture.
//!
//! Fixture files are stdlib-only Python so `process.exec` needs nothing but
//! the interpreter. All fixture commits use `env_clear`-compatible commands
//! (PATH + HOME only), mirroring the kernel's tool environment contract.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

pub struct FixtureRepo {
    dir: PathBuf,
}

/// Resolve `python3` from the current PATH (the kernel tool env allowlist
/// only carries what the harness passes, so the interpreter path is resolved
/// here once and embedded in the scripted plans).
pub fn python_path() -> String {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = Path::new(dir).join("python3");
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "/usr/bin/python3".into()
}

/// The minimal env allowlist the battery's `process.exec` calls carry.
pub fn tool_env() -> Value {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    json!({
        "PATH": format!("{path}"),
        "HOME": home,
    })
}

const CLAMP_BUGGY: &str = r#"def clamp(x, lo, hi):
    if x < lo:
        return lo
    if x > hi:
        return hi
    return lo
"#;

pub const CLAMP_FIXED: &str = r#"def clamp(x, lo, hi):
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x
"#;

const TEST_MATHLIB: &str = r#"import unittest
from mathlib import clamp

class ClampTests(unittest.TestCase):
    def test_low(self):
        self.assertEqual(clamp(-1, 0, 5), 0)

    def test_in_range(self):
        self.assertEqual(clamp(3, 0, 5), 3)

    def test_high(self):
        self.assertEqual(clamp(9, 0, 5), 5)

    def test_edges(self):
        self.assertEqual(clamp(0, 0, 5), 0)
        self.assertEqual(clamp(5, 0, 5), 5)

if __name__ == "__main__":
    unittest.main()
"#;

const MATHLIB_FEATURE: &str = r#"def clamp(x, lo, hi):
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x


def fib(n):
    raise NotImplementedError("fib lands in M7 task 2")
"#;

pub const MATHLIB_FIB: &str = r#"def clamp(x, lo, hi):
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x


def fib(n):
    if n < 2:
        return n
    a, b = 0, 1
    for _ in range(2, n + 1):
        a, b = b, a + b
    return b
"#;

const TEST_FIB: &str = r#"import unittest
from mathlib import clamp, fib

class ClampTests(unittest.TestCase):
    def test_in_range(self):
        self.assertEqual(clamp(3, 0, 5), 3)

class FibTests(unittest.TestCase):
    def test_base(self):
        self.assertEqual(fib(0), 0)
        self.assertEqual(fib(1), 1)

    def test_sequence(self):
        self.assertEqual([fib(n) for n in range(11)],
                         [0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55])

if __name__ == "__main__":
    unittest.main()
"#;

pub(crate) const CSVLIB: &str = r#"def parse_csv_line(line):
    fields = []
    cur = ""
    in_quotes = False
    i = 0
    while i < len(line):
        c = line[i]
        if c == '"':
            if in_quotes and i + 1 < len(line) and line[i + 1] == '"':
                cur += '"'
                i += 2
                continue
            in_quotes = not in_quotes
        elif c == "," and not in_quotes:
            fields.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    fields.append(cur)
    return fields
"#;

pub const CSVLIB_REFACTORED: &str = r#"def split_fields(line):
    fields = []
    cur = ""
    in_quotes = False
    i = 0
    while i < len(line):
        c = line[i]
        if c == '"':
            if in_quotes and i + 1 < len(line) and line[i + 1] == '"':
                cur += '"'
                i += 2
                continue
            in_quotes = not in_quotes
        elif c == "," and not in_quotes:
            fields.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    fields.append(cur)
    return fields


def unquote(field):
    if len(field) >= 2 and field[0] == '"' and field[-1] == '"':
        return field[1:-1].replace('""', '"')
    return field


def parse_csv_line(line):
    return [unquote(f) for f in split_fields(line)]
"#;

const TEST_CSVLIB: &str = r#"import unittest
from csvlib import parse_csv_line

class ParseCsvLineTests(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(parse_csv_line("a,b,c"), ["a", "b", "c"])

    def test_quoted_comma(self):
        self.assertEqual(parse_csv_line('a,"b,c",d'), ["a", "b,c", "d"])

    def test_escaped_quote(self):
        self.assertEqual(parse_csv_line('"say ""hi""",x'), ['say "hi"', "x"])

    def test_trailing_comma(self):
        self.assertEqual(parse_csv_line("a,"), ["a", ""])

    def test_empty(self):
        self.assertEqual(parse_csv_line(""), [""])

if __name__ == "__main__":
    unittest.main()
"#;

const STATE_PY: &str = r#"import json

STATE = "state.json"


def save_state(obj):
    with open(STATE, "w") as f:
        f.write(json.dumps(obj))


def load_state():
    with open(STATE) as f:
        return json.load(f)


def simulate_crash():
    # Torn tail: truncate mid-write, the buggy write path's window.
    with open(STATE, "w") as f:
        f.write('{"par')
"#;

const TEST_INTEGRATION: &str = r#"import unittest
import state

class RecoveryTests(unittest.TestCase):
    def test_recovery_after_crash(self):
        state.save_state({"version": 1, "items": [1, 2, 3]})
        state.simulate_crash()
        # A crash mid-write must not destroy the previous durable state.
        recovered = state.load_state()
        self.assertEqual(recovered["version"], 1)

if __name__ == "__main__":
    unittest.main()
"#;

const NOTES_A: &str = "alpha v1\n";
const NOTES_B: &str = "beta v1\n";

const TEST_SLOW: &str = r#"import unittest

class SlowTests(unittest.TestCase):
    def test_heavy_loop(self):
        # Deliberately slow: a CPU-bound fold that widens the crash window
        # for the interrupted-task harness (a long step is realistic work).
        total = 0
        for i in range(4_000_000):
            total += i % 7
        self.assertEqual(total % 7, 6)

if __name__ == "__main__":
    unittest.main()
"#;

impl FixtureRepo {
    pub(crate) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-dogfood-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main", "."]);
        repo.git(&["config", "user.email", "dogfood@kanbei.test"]);
        repo.git(&["config", "user.name", "kanbei dogfood"]);
        repo
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.dir.join(name), content).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    /// git under the same environment contract as the kernel tools.
    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap_or_else(|e| panic!("fixture git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Task 1: one deliberately broken commit — `clamp` returns `lo` for
/// in-range values; the failing test pins the correct behavior.
pub fn fixture_task1() -> FixtureRepo {
    let repo = FixtureRepo::new("t1");
    repo.write("mathlib.py", CLAMP_BUGGY);
    repo.write("test_mathlib.py", TEST_MATHLIB);
    repo.commit("clamp with an in-range bug");
    repo
}

/// Task 2: a precisely specified feature — `fib` is missing and the test
/// contract pins the sequence.
pub fn fixture_task2() -> FixtureRepo {
    let repo = FixtureRepo::new("t2");
    repo.write("mathlib.py", MATHLIB_FEATURE);
    repo.write("test_mathlib.py", TEST_FIB);
    repo.commit("mathlib with fib stubbed out");
    repo
}

/// Task 3: a single function with a full green suite (the refactor target).
pub fn fixture_task3() -> FixtureRepo {
    let repo = FixtureRepo::new("t3");
    repo.write("csvlib.py", CSVLIB);
    repo.write("test_csvlib.py", TEST_CSVLIB);
    repo.commit("csvlib parse_csv_line with green suite");
    repo
}

/// Task 4: an integration test failing from an obfuscated root cause — the
/// non-atomic `save_state` write path loses durable state to a torn tail.
pub fn fixture_task4() -> FixtureRepo {
    let repo = FixtureRepo::new("t4");
    repo.write("state.py", STATE_PY);
    repo.write("test_integration.py", TEST_INTEGRATION);
    repo.commit("state store with integration test");
    repo
}

/// Task 5 (part A repo): empty of features — `gcd` must be implemented and
/// committed, then a checkpoint taken for the part-B session.
pub fn fixture_task5() -> FixtureRepo {
    let repo = FixtureRepo::new("t5");
    repo.write(".keep", "");
    repo.commit("task 5 scratch repo");
    repo
}

/// Task 6: two committed notes; the interrupted task edits + commits both.
pub fn fixture_task6() -> FixtureRepo {
    let repo = FixtureRepo::new("t6");
    repo.write("notes_a.txt", NOTES_A);
    repo.write("notes_b.txt", NOTES_B);
    repo.write("test_slow.py", TEST_SLOW);
    repo.commit("task 6 baseline");
    repo
}

pub const NOTES_A_V2: &str = "alpha v2\n";
pub const NOTES_B_V2: &str = "beta v2\n";
