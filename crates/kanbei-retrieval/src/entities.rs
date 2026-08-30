//! Deterministic exact-entity extraction (architecture.md "Minimal retrieval
//! pipeline" step 2, R-12/M-03): conservative, pure, hand-rolled scanners
//! over claim text. No regex crate — every rule is a byte scan. The same
//! (key, kind) pair is emitted once, in first-seen text order.

/// The exact-entity kinds extracted from text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntityKind {
    /// Filesystem paths: `/abs`, `./rel`, `../rel`, or any `/`-containing
    /// token with a known source extension.
    Path,
    /// Rust-style paths (`A::B::C`) and backticked identifiers (`` `ident` ``).
    Symbol,
    /// Hex runs of 7-40 chars containing at least one `a-f` digit.
    Commit,
    /// Identifiers ending in `Error`/`error` and rustc codes (`E0425`).
    Error,
    /// Ticket references (`7AI-12345`, `KANBEI-9`).
    Ticket,
}

/// Known source extensions for the path rule (architecture.md R-12/M-03).
const PATH_EXTS: &[&str] = &[
    ".rs", ".md", ".toml", ".json", ".py", ".ts", ".js", ".lua", ".c", ".h", ".go", ".sh",
];

/// Leading delimiters stripped before classifying a whitespace run.
const LEADING_TRIM: &[u8] = b"([{\"'";
/// Trailing punctuation stripped from an extracted path key.
const TRAILING_TRIM: &[u8] = b".,;:)]}\"'";

/// An identifier character: alnum or underscore.
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Lowercase hex digit.
fn is_hex_lower(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// An identifier start character.
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Whitespace-delimited runs that look like paths: a `/`/`./`/`../` prefix,
/// or a `/` plus a known extension suffix (checked after stripping trailing
/// punctuation). A bare `notes.md` without a slash is not a path.
fn path_spans(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let mut s = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut e = i;
        while s < e && LEADING_TRIM.contains(&b[s]) {
            s += 1;
        }
        while e > s && TRAILING_TRIM.contains(&b[e - 1]) {
            e -= 1;
        }
        if e <= s {
            continue;
        }
        let tok = &text[s..e];
        let is_path = tok.starts_with('/')
            || tok.starts_with("./")
            || tok.starts_with("../")
            || (tok.contains('/') && PATH_EXTS.iter().any(|ext| tok.ends_with(ext)));
        if is_path {
            out.push((s, e, tok.to_string()));
        }
    }
    out
}

/// Rust-style paths (`A::B::C`, segments of alnum+`_`) and backticked
/// identifiers (`` `ident` ``, dots allowed inside). `::` spans are the
/// longest match — no sub-path keys.
fn symbol_spans(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b':' && b[i + 1] == b':' {
            let mut left = i;
            while left > 0 && is_word(b[left - 1]) {
                left -= 1;
            }
            let mut right = i + 2;
            while right < b.len() && is_word(b[right]) {
                right += 1;
            }
            // extend through further `::segment` parts (longest match)
            let mut end = right;
            while end + 2 < b.len() && b[end] == b':' && b[end + 1] == b':' && is_word(b[end + 2]) {
                end += 2;
                while end < b.len() && is_word(b[end]) {
                    end += 1;
                }
            }
            let first = b[left];
            let valid = left < i && right > i + 2 && (first.is_ascii_alphabetic() || first == b'_');
            if valid {
                out.push((left, end, text[left..end].to_string()));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    // backticked identifiers
    let mut j = 0;
    while j < b.len() {
        if b[j] == b'`' {
            let mut k = j + 1;
            while k < b.len() && b[k] != b'`' {
                k += 1;
            }
            if k < b.len() {
                let inner = &text[j + 1..k];
                let first = inner.as_bytes().first().copied();
                let ok = !inner.is_empty()
                    && inner
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
                    && first.is_some_and(is_ident_start);
                if ok {
                    out.push((j, k + 1, inner.to_string()));
                }
                j = k + 1;
                continue;
            }
        }
        j += 1;
    }
    out
}

/// Word-bounded hex runs of 7-40 chars with at least one `a-f` digit
/// (all-digit strings are numbers, not commits).
fn commit_spans(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_hex_lower(b[i]) {
            let start = i;
            while i < b.len() && is_hex_lower(b[i]) {
                i += 1;
            }
            let len = i - start;
            let bounded =
                (start == 0 || !is_word(b[start - 1])) && (i == b.len() || !is_word(b[i]));
            let has_letter = text[start..i].bytes().any(|c| matches!(c, b'a'..=b'f'));
            if (7..=40).contains(&len) && bounded && has_letter {
                out.push((start, i, text[start..i].to_string()));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Identifiers ending in `Error`/`error` (with at least one preceding char —
/// the bare word "error" is prose, not an entity) plus word-bounded rustc
/// codes `E\d{4}`.
fn error_spans(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_ident_start(b[i]) {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let tok = &text[start..i];
            let is_error_ident =
                (tok.ends_with("Error") || tok.ends_with("error")) && tok.len() > 5;
            let is_rustc_code = tok.len() == 5
                && tok.starts_with('E')
                && tok[1..].bytes().all(|c| c.is_ascii_digit())
                && (start == 0 || !is_word(b[start - 1]))
                && (i == b.len() || !is_word(b[i]));
            if is_error_ident || is_rustc_code {
                out.push((start, i, tok.to_string()));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Tickets: an optional leading digit, 2-6 uppercase letters, `-`, and 1-6
/// digits (`7AI-12345`, `KANBEI-9`), word-bounded.
fn ticket_spans(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let mut j = i;
        // optional leading digit, only when a letter run follows
        if j < b.len() && b[j].is_ascii_digit() && j + 1 < b.len() && b[j + 1].is_ascii_uppercase()
        {
            j += 1;
        }
        let letters_start = j;
        while j < b.len() && b[j].is_ascii_uppercase() {
            j += 1;
        }
        let letters = j - letters_start;
        if (2..=6).contains(&letters) && j < b.len() && b[j] == b'-' {
            let digits_start = j + 1;
            while j + 1 < b.len() && b[j + 1].is_ascii_digit() {
                j += 1;
            }
            let digits = j + 1 - digits_start;
            let bounded =
                (i == 0 || !is_word(b[i - 1])) && (j + 1 == b.len() || !is_word(b[j + 1]));
            if (1..=6).contains(&digits) && bounded {
                out.push((i, j + 1, text[i..j + 1].to_string()));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Deterministic exact-entity extraction: all rule matches merged in
/// first-seen text order, deduplicated by (key, kind).
pub fn extract_entities(text: &str) -> Vec<(String, EntityKind)> {
    let mut spans: Vec<(usize, usize, String, EntityKind)> = Vec::new();
    for (s, e, key) in path_spans(text) {
        spans.push((s, e, key, EntityKind::Path));
    }
    for (s, e, key) in symbol_spans(text) {
        spans.push((s, e, key, EntityKind::Symbol));
    }
    for (s, e, key) in commit_spans(text) {
        spans.push((s, e, key, EntityKind::Commit));
    }
    for (s, e, key) in error_spans(text) {
        spans.push((s, e, key, EntityKind::Error));
    }
    for (s, e, key) in ticket_spans(text) {
        spans.push((s, e, key, EntityKind::Ticket));
    }
    spans.sort_by_key(|(s, _, _, _)| *s);
    let mut out: Vec<(String, EntityKind)> = Vec::new();
    for (_, _, key, kind) in spans {
        if !out.iter().any(|(k, kd)| k == &key && *kd == kind) {
            out.push((key, kind));
        }
    }
    out
}

/// Just the normalized keys, deduplicated, first-seen order.
pub fn extract_entity_keys(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (key, _) in extract_entities(text) {
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// Lowercase with whitespace collapsed to single spaces — the shared
/// normalization for FTS fallback tokens and salience token overlap.
pub fn normalize_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<String> {
        extract_entity_keys(text)
    }

    #[test]
    fn paths_cover_prefixes_extensions_and_punctuation() {
        let k = keys("see ./x, /abs/path, ../rel and src/lib.rs; also notes at /tmp/x.md.");
        assert_eq!(
            k,
            vec![
                "./x".to_string(),
                "/abs/path".to_string(),
                "../rel".to_string(),
                "src/lib.rs".to_string(),
                "/tmp/x.md".to_string(),
            ]
        );
    }

    #[test]
    fn paths_need_a_slash() {
        // bare filenames and extension-less dotted tokens are not paths
        assert!(keys("notes.md and archive.tar.gz and main.rs here").is_empty());
    }

    #[test]
    fn symbols_cover_rust_paths_and_backticks() {
        let k = keys("call A::B::C and `ident` and `kanbei.provider`; no bare words");
        assert_eq!(
            k,
            vec![
                "A::B::C".to_string(),
                "ident".to_string(),
                "kanbei.provider".to_string(),
            ]
        );
        // `::` with empty sides is not a symbol
        assert!(keys("a :: b and ::x and y::").is_empty());
    }

    #[test]
    fn commits_are_hex_with_a_letter() {
        let k = keys("commit 1a2b3c4 then deadbeef01 but 1234567 is a number");
        assert_eq!(k, vec!["1a2b3c4".to_string(), "deadbeef01".to_string()]);
        // too short, too long, and word-embedded runs are skipped
        assert!(
            keys("abc123 and 1234567 plus a1234567890abcdef1234567890abcdef1234567890abcdef")
                .is_empty()
        );
    }

    #[test]
    fn errors_cover_identifiers_and_rustc_codes() {
        let k = keys("FooError at E0425; ParseError too; an error is just prose");
        assert_eq!(
            k,
            vec![
                "FooError".to_string(),
                "E0425".to_string(),
                "ParseError".to_string()
            ]
        );
        assert!(keys("E042 and E04255 and ERROR").is_empty());
    }

    #[test]
    fn tickets_cover_ratified_examples() {
        let k = keys("see 7AI-12345 and KANBEI-9 and AB-123");
        assert_eq!(
            k,
            vec![
                "7AI-12345".to_string(),
                "KANBEI-9".to_string(),
                "AB-123".to_string()
            ]
        );
        assert!(keys("abc-123 and A-1 and ABCDEFG-12 and KANBEI-1234567").is_empty());
    }

    #[test]
    fn plain_prose_yields_nothing() {
        assert!(
            keys("the quick brown fox jumps over the lazy dog while the build stays green")
                .is_empty()
        );
    }

    #[test]
    fn entities_dedup_by_key_and_kind() {
        let k = keys("a/b.rs then a/b.rs again and `x` plus `x`");
        assert_eq!(k, vec!["a/b.rs".to_string(), "x".to_string()]);
    }

    #[test]
    fn query_normalization_lowercases_and_collapses() {
        assert_eq!(normalize_query("  Foo\tBar\nBaz  "), "foo bar baz");
        assert_eq!(normalize_query(""), "");
    }
}
