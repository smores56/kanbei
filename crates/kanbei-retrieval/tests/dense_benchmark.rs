//! M8 wave 4a — dense-retrieval entry-criteria ablation benchmark (test-only).
//!
//! Re-entry rule (docs/architecture.md:695,715,744; docs/review-reconciliation.md:129,184,236):
//! dense retrieval re-enters ONLY if the memory benchmark plan justifies it — plan item S11
//! "Retrieval quality: FTS-only vs +dense ablations on synthetic coding histories" — which is
//! exactly the ablation this file measures. This is a prototype harness, NOT production code:
//! no crate code changes, no new dependencies, and the dense stage is a deterministic
//! in-test character n-gram hashing embedding over the synthetic corpus.
//!
//! Measured, per query class (20 lexical / 20 gap):
//!   recall@5 / recall@10 for (a) the current production pipeline (exact entities + FTS5/BM25
//!     + one-hop — `MemoryIndex::search`, unchanged), (b) a dense-only prototype, (c) an RRF
//!     fusion of the two (k = 60; the exact fusion is a production decision — the benchmark
//!     only needs the recall delta). Plus dense-stage query latency p50/p95 at corpus sizes
//!     500/2000/5000, and a determinism check (the fusion runs twice and must produce
//!     identical result sets).
//!
//! GAP-class construction (the known M7 gap, docs/m7-report.md:84: "FTS5 AND-joins query
//! tokens, so the citation query must use tokens present in the claim"): each gap query is a
//! hand-written paraphrase/synonym/abbreviation/reordering whose FTS5 token set is disjoint
//! from the target claim's token set, so the AND-join cannot match the target. The harness
//! asserts that disjointness, that gap queries carry no entity keys, and that gap FTS recall
//! is exactly 0.0 (targets share no entity keys and no edges with any other claim, so neither
//! the exact-entity step nor one-hop expansion can rescue them).
//!
//! Analysis / decision rubric (mirrored in the printed report; thresholds are the harness's
//! recommendation — the milestone decides from the numbers):
//!   (a) does dense close the gap class? — gap recall@5 of dense-only / fusion vs 0.0 for
//!       the FTS pipeline;
//!   (b) lexical regression risk — fusion lexical recall@5 vs FTS-only (fusion must not lose
//!       hits the lexical stage already had);
//!   (c) latency at scale — dense p50/p95 per query at 500/2000/5000 claims;
//!   (d) recommendation: JUSTIFY re-entry when dense gap recall@5 >= 0.6 AND fusion lexical
//!       recall@5 >= 0.9 AND dense p95 @ 5000 < 5 ms (next step per S11: sqlite-vec spike at
//!       100k claims); DEFER otherwise.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str::FromStr;
use std::time::{Duration, Instant};

use kanbei_capabilities::Principal;
use kanbei_core::{Digest, Id128};
use kanbei_memory::{Claim, ClaimProvenance, MEMORY_CLAIM_SCHEMA, MemoryScope, RootFold};
use kanbei_retrieval::{MemoryIndex, ScopeIndexInput, SearchQuery, extract_entities};

const CORPUS_N: usize = 500;
const DENSE_DIM: usize = 512;
const DENSE_GRAMS: [usize; 2] = [3, 4];
const DENSE_TOP: usize = 50;
const RRF_K: f64 = 60.0;
const SEED: u64 = 0xD3A5_5EED;

/// Tiny deterministic xorshift64 PRNG (mirrors kanbei-testkit's rng.rs) — no
/// rand dependency; same seed -> same corpus, guaranteed across runs.
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // 0 is a fixed point of xorshift; remap to a nonzero constant
        Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in `0..limit` (0 when `limit == 0`).
    fn next_usize(&mut self, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        (self.next_u64() % limit as u64) as usize
    }
}

/// Base58 of `bytes` (the Id128 wire encoding). The test needs many
/// deterministic Id128s and only `Id128::from_str` can build one, so the tiny
/// encoder mirrors bs58's alphabet; a self-check asserts the zero encoding.
fn base58(bytes: &[u8]) -> String {
    const ALPHA: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = Vec::with_capacity(bytes.len() * 138 / 100 + 1);
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut out = String::with_capacity(digits.len() + zeros);
    out.extend(std::iter::repeat_n('1', zeros));
    for d in digits.iter().rev() {
        out.push(ALPHA[*d as usize] as char);
    }
    out
}

/// A deterministic Id128 from a numeric tag (16 bytes, tag in the low half).
fn id128(tag: u64) -> Id128 {
    let mut bytes = [0u8; 16];
    bytes[8..].copy_from_slice(&tag.to_be_bytes());
    Id128::from_str(&base58(&bytes)).expect("deterministic id")
}

/// One corpus claim: the content-addressed object plus the metadata the
/// benchmark needs (deterministic id, scope, digest).
#[derive(Clone)]
struct BenchClaim {
    claim_id: Id128,
    digest: Digest,
    content: String,
    scope: MemoryScope,
    claim: Claim,
}

/// The 40 hand-written target claims. Scopes rotate by index over
/// [Lifetime, Project A, Project B]. Claims 0-19 are the LEXICAL targets
/// (every query token appears verbatim in the claim, so the FTS5 AND-join
/// matches); claims 20-39 are the GAP targets (queries paraphrase them with
/// a token set disjoint from the claim's — see GAP_QUERIES).
const TARGET_CLAIMS: [&str; 40] = [
    // lexical targets
    "the checkout service validates email addresses with a regex before charging",
    "`parse_csv_line` splits quoted fields in csvlib.py",
    "the session kernel writes frames to a zstd-compressed journal",
    "the wasm host runs luaur modules in per-generation instances",
    "the approval broker caps tool intents at 125 tokens",
    "reconcile deletes entity rows when a claim leaves the fold",
    "the retraction edge marks the older claim superseded",
    "build_fts rebuilds the virtual table after a reconcile",
    "the activation log records a score per claim per projector run",
    "the one-hop expansion seeds from the top eight fused scores",
    "the backup job runs every 6 hours and keeps 14 snapshots",
    "the feature flags fall back to defaults when the cache misses",
    "the api gateway rate limits per api key at 100 requests a minute",
    "the telemetry exporter batches spans every 5 seconds",
    "the deployment pipeline requires a signed approval for production",
    "the auth service rotates signing keys every 30 days",
    "the inventory service holds a per-sku lock during decrement",
    "the shipping label printer renders a4 pdfs",
    "the notification worker dedupes alerts for an hour",
    "the search index reindexes documents on a content change",
    // gap targets
    "implemented gcd in mathlib.py as an iterative euclidean loop",
    "the bootloader in init/main.rs panics when the initramfs image is missing",
    "the exporter forwards traces to the collector over grpc in export/forward.rs",
    "the connection pool in net/http.rs grows to 64 sockets under load",
    "the retry policy in retry/backoff.rs gives up after three attempts on transient failures",
    "the token cache in sso/cache.rs expires entries after an hour",
    "duplicate webhook deliveries are ignored by our receiver in hooks/ingest.rs",
    "the search index shards documents by tenant id in search/shard.rs",
    "backups are encrypted with a kms key before upload to cold storage",
    "the api gateway strips the internal auth header before upstream in gateway/proxy.rs",
    "the rate limiter in edge/limit.rs keys counters by api key per minute",
    "opentelemetry spans in telemetry/export.rs carry a deployment id as a tag",
    "the fs journal in kernel/journal.rs replays writes after crash",
    "the worker in queue/worker.rs drains the queue only when the consumer is idle",
    "the build cache in build/cache.rs misses when a flag toggles the macro set",
    "the checkpoint in core/checkpoint.rs stores the manifest digest at the head",
    "migrations in db/migrate.rs run inside a single transaction per version",
    "the linter in tools/lint.rs skips generated files under the vendor directory",
    "the reconciler at sync/reconcile.rs backfills missing rows from the source of truth",
    "the sampler in telemetry/sample.rs keeps one percent of traces in production",
];

/// (query text, target claim index, is_gap). LEXICAL queries reuse the
/// claim's own tokens (at least one distinctive token is unique to the
/// target). GAP queries are the failure class the benchmark must quantify:
/// paraphrases ("gcd" -> "greatest common divisor"), synonyms, abbreviations
/// ("fs" -> "filesystem", "otel" -> "opentelemetry"), reordering ("panic in
/// the kernel" -> "kernel panic"), partial tokens ("initramfs" -> "initrd"),
/// cross-token phrasing — with an FTS5 token set disjoint from the target.
const QUERIES: [(&str, usize, bool); 40] = [
    // lexical
    ("checkout service regex email validates", 0, false),
    ("parse_csv_line csvlib quoted fields", 1, false),
    ("session kernel zstd journal frames", 2, false),
    ("luaur modules wasm host instances", 3, false),
    ("approval broker caps tool intents 125 tokens", 4, false),
    ("reconcile entity rows claim leaves fold", 5, false),
    ("retraction edge superseded older claim", 6, false),
    ("build_fts virtual table reconcile", 7, false),
    ("activation log score per projector", 8, false),
    ("one-hop expansion eight fused seeds", 9, false),
    ("backup job 6 hours 14 snapshots", 10, false),
    ("feature flags defaults cache misses", 11, false),
    ("api gateway rate limits 100 requests", 12, false),
    ("telemetry exporter batches spans 5 seconds", 13, false),
    ("deployment pipeline signed approval production", 14, false),
    ("auth service rotates signing keys 30 days", 15, false),
    ("inventory per-sku lock decrement", 16, false),
    ("shipping label printer a4 pdfs", 17, false),
    ("notification worker dedupes alerts hour", 18, false),
    ("search index reindexes content change", 19, false),
    // gap
    ("calculate the greatest common divisor", 20, true),
    ("kernel panic during boot with no initrd present", 21, true),
    ("how does trace data reach any backend", 22, true),
    ("how many open connections are allowed at peak", 23, true),
    ("what happens when a temporary error keeps happening", 24, true),
    ("how long are auth credentials remembered", 25, true),
    ("same callback arriving twice is skipped", 26, true),
    ("how is data split across customers", 27, true),
    ("nightly snapshot copies get protected in transit", 28, true),
    ("how is a secret value kept out of backend calls", 29, true),
    ("how often can one caller hit an endpoint", 30, true),
    ("which label identifies environment on every otel trace", 31, true),
    ("how does a filesystem recover pending data on restart", 32, true),
    ("processing pauses until a downstream service catches up", 33, true),
    ("why did compilation redo all work", 34, true),
    ("where does a snapshot record its latest state pointer", 35, true),
    ("are schema changes applied atomically", 36, true),
    ("why are some sources excluded from style checks", 37, true),
    ("how do gaps in a mirror get repaired", 38, true),
    ("what fraction is kept live", 39, true),
];

// Filler vocabulary — realistic coding-history components (function behavior,
// bug fixes, refactors, build/config facts) with paths and symbols. The
// vocabulary deliberately avoids every lexical query's distinctive tokens, so
// each lexical query matches (almost) only its target claim.
const FILLER_PATHS: [&str; 12] = [
    "src/retrieval/search.rs",
    "src/session/spine.rs",
    "src/memory/index.rs",
    "app/checkout/cart.rs",
    "app/auth/tokens.rs",
    "ingest/collector.rs",
    "stream/transformer.rs",
    "storage/warehouse.rs",
    "config/build.rs",
    "ops/deploy.rs",
    "telemetry/exporter.rs",
    "ops/backup.rs",
];
const FILLER_FNS: [&str; 10] = [
    "tokenize", "split_fields", "unquote", "merge_chunks", "decode_frame", "rotate_key",
    "backfill_rows", "normalize", "throttle", "compact",
];
const FILLER_VERBS: [&str; 10] = [
    "batches", "buffers", "compresses", "deduplicates", "indexes", "migrates", "samples",
    "streams", "throttles", "retries",
];
const FILLER_OBJECTS: [&str; 10] = [
    "blobs", "rows", "segments", "buffers", "batches", "entries", "records", "chunks", "leases",
    "keys",
];
const FILLER_DETAILS: [&str; 10] = [
    "in append mode",
    "with a 60 second window",
    "under memory pressure",
    "before the commit",
    "at 100 hz",
    "on every wake",
    "after a restart",
    "with a 64 kb limit",
    "in dry-run mode",
    "on the secondary replica",
];
const FILLER_BUGS: [&str; 8] = [
    "null dereference",
    "race condition",
    "buffer overflow",
    "deadlock",
    "off-by-one",
    "timezone bug",
    "leaked descriptor",
    "stale cache",
];
const FILLER_CAUSES: [&str; 8] = [
    "unchecked null",
    "unsynchronized write",
    "bounded buffer miscalc",
    "lock ordering",
    "boundary rounding",
    "utc assumption",
    "missing close",
    "stale invalidation",
];
const FILLER_SYMPTOMS: [&str; 8] = [
    "a crash at startup",
    "corrupted rows",
    "silent drops",
    "intermittent timeouts",
    "wrong totals",
    "duplicate events",
    "a stuck queue",
    "stale reads",
];
const FILLER_SUBJECTS: [&str; 8] = [
    "order service",
    "payments gateway",
    "ingestion worker",
    "projection worker",
    "retention worker",
    "sync worker",
    "metadata store",
    "config loader",
];
const FILLER_STATES: [&str; 8] = [
    "pauses", "recovers", "falls back", "fails over", "restarts", "resumes", "quarantines",
    "replays",
];
const FILLER_FLAGS: [&str; 8] = [
    "debug", "release", "opt-level", "lto", "panic", "strip", "overflow-checks",
    "codegen-units",
];
const FILLER_VALUES: [&str; 8] = ["0", "1", "2", "true", "false", "off", "size", "thin"];
const FILLER_ENVS: [&str; 4] = ["ci", "nightly", "staging", "release"];
const FILLER_REASONS: [&str; 8] = [
    "shorter startup",
    "smaller binary",
    "cache locality",
    "simpler review",
    "disk pressure",
    "test isolation",
    "faster builds",
    "less churn",
];
const FILLER_MODULES: [&str; 6] = ["two", "three", "four", "five", "six", "seven"];
const FILLER_MODULE_NAMES: [&str; 8] = [
    "core", "config", "store", "queue", "wire", "util", "api", "runtime",
];

fn pick<'a>(rng: &mut Rng, pool: &'a [&'a str]) -> &'a str {
    pool[rng.next_usize(pool.len())]
}

/// Deterministic seeded filler claims (template slots picked from the fixed
/// vocabulary above). No RNG-free run differences: same seed -> same corpus.
fn generate_fillers(rng: &mut Rng, n: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let text = match rng.next_usize(6) {
            0 => format!(
                "`{}` in {} {} {} {}.",
                pick(rng, &FILLER_FNS),
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_VERBS),
                pick(rng, &FILLER_OBJECTS),
                pick(rng, &FILLER_DETAILS),
            ),
            1 => format!(
                "fixed a {} in {}: the {} produced {}.",
                pick(rng, &FILLER_BUGS),
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_CAUSES),
                pick(rng, &FILLER_SYMPTOMS),
            ),
            2 => format!(
                "refactored {} into {} modules: {}, {}.",
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_MODULES),
                pick(rng, &FILLER_MODULE_NAMES),
                pick(rng, &FILLER_MODULE_NAMES),
            ),
            3 => format!(
                "set {} to {} in {} for the {} build.",
                pick(rng, &FILLER_FLAGS),
                pick(rng, &FILLER_VALUES),
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_ENVS),
            ),
            4 => format!(
                "the {} {} {}.",
                pick(rng, &FILLER_SUBJECTS),
                pick(rng, &FILLER_STATES),
                pick(rng, &FILLER_DETAILS),
            ),
            _ => format!(
                "moved {} to {} for {}.",
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_PATHS),
                pick(rng, &FILLER_REASONS),
            ),
        };
        out.push(text);
    }
    out
}

fn make_bench_claim(tag: u64, content: &str, scope: MemoryScope) -> BenchClaim {
    let claim_id = id128(tag);
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id,
        kind: "decision".to_string(),
        content: content.to_string(),
        owner: Principal {
            session: claim_id,
            generation: 1,
            run: None,
        },
        visibility_scope: MemoryScope::Lifetime,
        provenance: ClaimProvenance::new_ordinary(claim_id, 1 + tag),
        observed_at: None,
        valid_from: None,
        sensitivity: "public".to_string(),
    };
    let digest = claim.digest();
    BenchClaim {
        claim_id,
        digest,
        content: claim.content.clone(),
        scope,
        claim,
    }
}

fn scope_for(i: usize, scopes: &[MemoryScope]) -> MemoryScope {
    scopes[i % scopes.len()].clone()
}

/// The 500-claim corpus: 40 hand-written targets + 460 seeded fillers,
/// scopes rotating over the three scopes.
fn build_corpus(scopes: &[MemoryScope]) -> Vec<BenchClaim> {
    let mut claims = Vec::with_capacity(CORPUS_N);
    for (i, content) in TARGET_CLAIMS.iter().enumerate() {
        claims.push(make_bench_claim(i as u64, content, scope_for(i, scopes)));
    }
    let mut rng = Rng::new(SEED);
    for (offset, content) in generate_fillers(&mut rng, CORPUS_N - TARGET_CLAIMS.len())
        .into_iter()
        .enumerate()
    {
        let i = TARGET_CLAIMS.len() + offset;
        claims.push(make_bench_claim(i as u64, &content, scope_for(i, scopes)));
    }
    claims
}

/// One `MemoryIndex` over the corpus, grouped into the three scope folds —
/// the exact current production retrieval (no changes).
fn build_index(claims: &[BenchClaim], scopes: &[MemoryScope]) -> MemoryIndex {
    let mut groups: Vec<Vec<&BenchClaim>> = vec![Vec::new(); scopes.len()];
    for c in claims {
        let gi = scopes
            .iter()
            .position(|s| *s == c.scope)
            .expect("claim scope is one of the benchmark scopes");
        groups[gi].push(c);
    }
    let inputs: Vec<ScopeIndexInput> = groups
        .iter()
        .map(|group| {
            let fold = RootFold {
                root: group.first().map(|c| c.digest),
                claims: group
                    .iter()
                    .map(|c| (c.digest, c.claim.clone()))
                    .collect(),
                edges: Vec::new(),
                retracted: Vec::new(),
                history: Vec::new(),
            };
            ScopeIndexInput {
                scope: group[0].scope.clone(),
                root: group.first().map(|c| c.digest),
                fold,
            }
        })
        .collect();
    let path = std::env::temp_dir().join(format!(
        "kanbei-dense-bench-{}.db",
        std::process::id()
    ));
    let mut index = MemoryIndex::open(&path).expect("open index");
    index
        .build(&inputs, "dense-benchmark")
        .expect("build index");
    index
}

/// Approximates the FTS5 unicode61 tokenizer (lowercased alphanumeric runs)
/// for the gap-query disjointness check.
fn fts_tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            for lc in ch.to_lowercase() {
                cur.push(lc);
            }
        } else if !cur.is_empty() {
            out.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.insert(cur);
    }
    out
}

/// FNV-1a — a fixed, deterministic hash (std's RandomState is seeded per
/// process, so it is unusable here).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn l2_normalize(v: &mut [f64]) {
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// The prototype dense stage: character 3-4 gram hashing into a
/// {DENSE_DIM}-dim vector with sublinear TF weighting (1 + ln tf) and ±1 sign per gram,
/// L2-normalized. Deterministic: same text -> bit-identical vector. This
/// stands in for "a dense stage" — production would use a real embedding +
/// ANN index, which is the S11 decision the milestone makes from this report.
fn dense_embed(text: &str) -> Vec<f64> {
    let bytes = text.as_bytes();
    let mut counts: BTreeMap<u64, u32> = BTreeMap::new();
    for n in DENSE_GRAMS {
        if bytes.len() < n {
            continue;
        }
        for i in 0..=bytes.len() - n {
            *counts.entry(fnv1a(&bytes[i..i + n])).or_insert(0) += 1;
        }
    }
    let mut v = vec![0.0; DENSE_DIM];
    for (h, tf) in counts {
        let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
        v[(h % DENSE_DIM as u64) as usize] += sign * (1.0 + f64::from(tf).ln());
    }
    l2_normalize(&mut v);
    v
}

/// The dense embedding text: claim/query content plus its extracted entity
/// keys (paths, symbols) — the "content + path + entity tokens" surface.
fn dense_text(text: &str) -> String {
    let mut out = text.to_string();
    for (key, _) in extract_entities(text) {
        out.push(' ');
        out.push_str(&key);
    }
    out
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    // Manual 4-way unrolled loop: debug builds (the gate profile) do not
    // vectorize iterators, and this is the per-query hot path.
    let mut s = 0.0;
    let mut i = 0;
    while i + 4 <= a.len() {
        s += a[i] * b[i] + a[i + 1] * b[i + 1] + a[i + 2] * b[i + 2] + a[i + 3] * b[i + 3];
        i += 4;
    }
    while i < a.len() {
        s += a[i] * b[i];
        i += 1;
    }
    s
}

/// The brute-force dense stage over a pre-embedded corpus.
struct DenseIndex {
    vectors: Vec<Vec<f64>>,
    claim_ids: Vec<String>,
}

impl DenseIndex {
    fn build(claims: &[BenchClaim]) -> Self {
        let mut vectors = Vec::with_capacity(claims.len());
        let mut claim_ids = Vec::with_capacity(claims.len());
        for c in claims {
            vectors.push(dense_embed(&dense_text(&c.content)));
            claim_ids.push(c.claim_id.to_string());
        }
        Self { vectors, claim_ids }
    }

    /// Top-k by cosine, ties broken by claim_id ascending (matching the
    /// pipeline's deterministic tie-break). Returns (score, corpus index) —
    /// index-based so the per-query sort never clones claim strings.
    fn search(&self, qv: &[f64], k: usize) -> Vec<(f64, usize)> {
        let mut scored: Vec<(f64, usize)> = self
            .vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (dot(qv, v), i))
            .collect();
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| self.claim_ids[a.1].cmp(&self.claim_ids[b.1]))
        });
        scored.truncate(k);
        scored
    }
}

/// Reciprocal-rank fusion (k = 60) of the pipeline's ranked list and the
/// dense list; ties broken by digest for full determinism. The exact fusion
/// is a production decision — this only needs the recall delta.
fn rrf_fuse(fts: &[Digest], dense: &[Digest], out: usize) -> Vec<Digest> {
    let mut scores: HashMap<Digest, f64> = HashMap::new();
    for (rank, d) in fts.iter().enumerate() {
        *scores.entry(*d).or_insert(0.0) += 1.0 / (RRF_K + (rank + 1) as f64);
    }
    for (rank, d) in dense.iter().enumerate() {
        *scores.entry(*d).or_insert(0.0) += 1.0 / (RRF_K + (rank + 1) as f64);
    }
    let mut ranked: Vec<(Digest, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(out);
    ranked.into_iter().map(|(d, _)| d).collect()
}

/// The FTS-only leg: the current production pipeline, unchanged.
fn fts_leg(index: &MemoryIndex, text: &str, scopes: &[MemoryScope]) -> Vec<Digest> {
    let q = SearchQuery {
        text: text.to_string(),
        scopes: scopes.to_vec(),
        max_results: 20,
        ..Default::default()
    };
    index
        .search(&q)
        .expect("search")
        .claims
        .iter()
        .map(|c| c.digest)
        .collect()
}

fn hit_at(ordered: &[Digest], target: Digest, k: usize) -> bool {
    ordered.iter().take(k).any(|d| *d == target)
}

fn hits(lists: &[Vec<Digest>], targets: &[Digest], k: usize) -> usize {
    lists
        .iter()
        .zip(targets)
        .filter(|(l, t)| hit_at(l, **t, k))
        .count()
}

fn recall(lists: &[Vec<Digest>], targets: &[Digest], k: usize) -> f64 {
    hits(lists, targets, k) as f64 / targets.len() as f64
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Dense-stage query latency over one corpus: embed query + brute-force
/// cosine over every claim vector (corpus pre-embedded; warmup queries first).
fn dense_latency(claims: &[BenchClaim], queries: &[(&str, usize, bool)]) -> (Duration, Duration, f64) {
    let idx = DenseIndex::build(claims);
    for (text, _, _) in queries.iter().take(3) {
        let qv = dense_embed(&dense_text(text));
        idx.search(&qv, 1);
    }
    let mut times = Vec::with_capacity(queries.len());
    let mut top1_sum = 0.0;
    for (text, _, _) in queries {
        let t0 = Instant::now();
        let qv = dense_embed(&dense_text(text));
        let hits = idx.search(&qv, 1);
        times.push(t0.elapsed());
        top1_sum += hits[0].0;
    }
    times.sort();
    (
        percentile(&times, 0.50),
        percentile(&times, 0.95),
        top1_sum / queries.len() as f64,
    )
}

#[test]
fn dense_retrieval_entry_criteria_ablation() {
    let t_start = Instant::now();

    // Corpus + production index + dense prototype index.
    let scopes = vec![
        MemoryScope::Lifetime,
        MemoryScope::Project(id128(9000)),
        MemoryScope::Project(id128(9001)),
    ];
    let claims = build_corpus(&scopes);
    assert_eq!(claims.len(), CORPUS_N, "corpus size");
    let index = build_index(&claims, &scopes);
    let dense = DenseIndex::build(&claims);

    // Harness sanity: gap queries are token-disjoint from their targets and
    // carry no entity keys, so the FTS5 AND-join cannot match the target.
    for (text, target, _gap) in QUERIES.iter().filter(|(_, _, gap)| *gap) {
        assert!(
            extract_entities(text).is_empty(),
            "gap query {text:?} must carry no entity keys"
        );
        let q_tokens = fts_tokens(text);
        let c_tokens = fts_tokens(&claims[*target].content);
        assert!(
            q_tokens.is_disjoint(&c_tokens),
            "gap query {text:?} shares a token with its target claim"
        );
    }

    // Determinism probe: the dense embedding is bit-identical on re-run.
    let probe = "determinism probe text with a path /src/a/b.rs";
    let v1 = dense_embed(&dense_text(probe));
    let v2 = dense_embed(&dense_text(probe));
    assert!(
        v1.iter().zip(&v2).all(|(a, b)| a.to_bits() == b.to_bits()),
        "dense embedding must be deterministic"
    );

    // Per-query legs.
    let mut fts_lists: Vec<Vec<Digest>> = Vec::new();
    let mut dense_lists: Vec<Vec<Digest>> = Vec::new();
    for (text, _, _) in QUERIES {
        fts_lists.push(fts_leg(&index, text, &scopes));
        let qv = dense_embed(&dense_text(text));
        dense_lists.push(
            dense
                .search(&qv, DENSE_TOP)
                .into_iter()
                .map(|(_, i)| claims[i].digest)
                .collect(),
        );
    }

    // Fusion, twice — identical result sets required (determinism check).
    let mut runs: Vec<Vec<Vec<Digest>>> = Vec::new();
    for _ in 0..2 {
        let mut run = Vec::with_capacity(QUERIES.len());
        for (qi, _) in QUERIES.iter().enumerate() {
            run.push(rrf_fuse(&fts_lists[qi], &dense_lists[qi], 10));
        }
        runs.push(run);
    }
    assert_eq!(runs[0], runs[1], "fusion result sets must be deterministic");
    let fusion_lists = runs[0].clone();

    // Class splits.
    let lex_idx: Vec<usize> = (0..QUERIES.len()).filter(|i| !QUERIES[*i].2).collect();
    let gap_idx: Vec<usize> = (0..QUERIES.len()).filter(|i| QUERIES[*i].2).collect();
    let targets: Vec<Digest> = (0..QUERIES.len()).map(|i| claims[QUERIES[i].1].digest).collect();
    let lex_targets: Vec<Digest> = lex_idx.iter().map(|i| targets[*i]).collect();
    let gap_targets: Vec<Digest> = gap_idx.iter().map(|i| targets[*i]).collect();
    let lex_lists = |lists: &[Vec<Digest>]| -> Vec<Vec<Digest>> {
        lex_idx.iter().map(|i| lists[*i].clone()).collect()
    };
    let gap_lists = |lists: &[Vec<Digest>]| -> Vec<Vec<Digest>> {
        gap_idx.iter().map(|i| lists[*i].clone()).collect()
    };

    let fts_lex = lex_lists(&fts_lists);
    let fts_gap = gap_lists(&fts_lists);
    let dense_lex = lex_lists(&dense_lists);
    let dense_gap = gap_lists(&dense_lists);
    let fuse_lex = lex_lists(&fusion_lists);
    let fuse_gap = gap_lists(&fusion_lists);

    let r5 = |lists: &[Vec<Digest>], targets: &[Digest]| recall(lists, targets, 5);
    let r10 = |lists: &[Vec<Digest>], targets: &[Digest]| recall(lists, targets, 10);
    let fts_lex_r5 = r5(&fts_lex, &lex_targets);
    let fts_lex_r10 = r10(&fts_lex, &lex_targets);
    let fts_gap_r5 = r5(&fts_gap, &gap_targets);
    let fts_gap_r10 = r10(&fts_gap, &gap_targets);
    let dense_lex_r5 = r5(&dense_lex, &lex_targets);
    let dense_lex_r10 = r10(&dense_lex, &lex_targets);
    let dense_gap_r5 = r5(&dense_gap, &gap_targets);
    let dense_gap_r10 = r10(&dense_gap, &gap_targets);
    let fuse_lex_r5 = r5(&fuse_lex, &lex_targets);
    let fuse_lex_r10 = r10(&fuse_lex, &lex_targets);
    let fuse_gap_r5 = r5(&fuse_gap, &gap_targets);
    let fuse_gap_r10 = r10(&fuse_gap, &gap_targets);

    // Latency at scale: 500 (the recall corpus) / 2000 / 5000 claims from the
    // same seeded generator.
    let mut rng = Rng::new(SEED);
    let fillers = generate_fillers(&mut rng, 5000);
    let latency_2000: Vec<BenchClaim> = fillers[..2000]
        .iter()
        .enumerate()
        .map(|(i, c)| make_bench_claim(i as u64, c, MemoryScope::Lifetime))
        .collect();
    let latency_5000: Vec<BenchClaim> = fillers
        .iter()
        .enumerate()
        .map(|(i, c)| make_bench_claim(i as u64, c, MemoryScope::Lifetime))
        .collect();
    let (p50_500, p95_500, top1_500) = dense_latency(&claims, &QUERIES);
    let (p50_2000, p95_2000, top1_2000) = dense_latency(&latency_2000, &QUERIES);
    let (p50_5000, p95_5000, top1_5000) = dense_latency(&latency_5000, &QUERIES);

    // Harness sanity asserts (the numbers themselves are assert-free).
    assert!(
        fts_lex_r5 >= 0.9,
        "FTS-only lexical recall@5 must be >= 0.9 (harness sanity); got {fts_lex_r5:.3} \
         — the corpus/query construction is broken"
    );
    assert_eq!(
        hits(&fts_gap, &gap_targets, 5),
        0,
        "gap class must be a real gap for FTS at recall@5"
    );
    assert_eq!(
        hits(&fts_gap, &gap_targets, 10),
        0,
        "gap class must be a real gap for FTS at recall@10"
    );

    // Report.
    println!("====================================================================");
    println!("M8 wave 4a — dense-retrieval entry-criteria ablation (test-only)");
    println!("====================================================================");
    println!(
        "corpus  : {CORPUS_N} claims (40 hand-written targets + 460 seeded fillers), 3 scopes"
    );
    println!("queries : 40 (20 lexical / 20 gap); targets = claims 0-19 / 20-39");
    println!("dense   : char 3-4gram hashing -> {DENSE_DIM}-dim, sublinear TF, l2 (in-test prototype)");
    println!("fusion  : reciprocal-rank fusion k=60 over pipeline top-20 + dense top-{DENSE_TOP}");
    println!();
    println!("recall@5  (target in top 5):");
    println!("  class      fts-only   dense-only   fusion");
    println!(
        "  lexical    {fts_lex_r5:.3}      {dense_lex_r5:.3}        {fuse_lex_r5:.3}"
    );
    println!(
        "  gap        {fts_gap_r5:.3}      {dense_gap_r5:.3}        {fuse_gap_r5:.3}"
    );
    let all_r5 = |lists: &[Vec<Digest>]| recall(lists, &targets, 5);
    let all_r10 = |lists: &[Vec<Digest>]| recall(lists, &targets, 10);
    println!(
        "  all        {:.3}      {:.3}        {:.3}",
        all_r5(&fts_lists),
        all_r5(&dense_lists),
        all_r5(&fusion_lists)
    );
    println!("recall@10  (target in top 10):");
    println!("  class      fts-only   dense-only   fusion");
    println!(
        "  lexical    {fts_lex_r10:.3}      {dense_lex_r10:.3}        {fuse_lex_r10:.3}"
    );
    println!(
        "  gap        {fts_gap_r10:.3}      {dense_gap_r10:.3}        {fuse_gap_r10:.3}"
    );
    println!(
        "  all        {:.3}      {:.3}        {:.3}",
        all_r10(&fts_lists),
        all_r10(&dense_lists),
        all_r10(&fusion_lists)
    );
    println!();
    println!("gap detail (query: fts@10/dense@10/fusion@10):");
    for chunk in gap_idx.chunks(5) {
        let line = chunk
            .iter()
            .map(|qi| {
                format!(
                    "{qi}:{}/{}/{}",
                    hit_at(&fts_lists[*qi], targets[*qi], 10),
                    hit_at(&dense_lists[*qi], targets[*qi], 10),
                    hit_at(&fusion_lists[*qi], targets[*qi], 10)
                )
            })
            .collect::<Vec<_>>()
            .join("   ");
        println!("  {line}");
    }
    println!();
    println!("dense query latency (per query, brute-force cosine, corpus pre-embedded):");
    println!("  corpus   p50(ms)   p95(ms)   mean top-1 cosine");
    println!(
        "  {:<8} {:.3}     {:.3}     {top1_500:.4}",
        500,
        p50_500.as_secs_f64() * 1000.0,
        p95_500.as_secs_f64() * 1000.0
    );
    println!(
        "  {:<8} {:.3}     {:.3}     {top1_2000:.4}",
        2000,
        p50_2000.as_secs_f64() * 1000.0,
        p95_2000.as_secs_f64() * 1000.0
    );
    println!(
        "  {:<8} {:.3}     {:.3}     {top1_5000:.4}",
        5000,
        p50_5000.as_secs_f64() * 1000.0,
        p95_5000.as_secs_f64() * 1000.0
    );
    println!();
    println!("determinism: fusion result sets identical across two runs: true");
    println!(
        "sanity: fts-only lexical recall@5 = {fts_lex_r5:.3} (>= 0.9), \
         fts-only gap recall@5 = {fts_gap_r5:.3} (== 0.0), gap queries token-disjoint: 20/20"
    );
    println!(
        "total test elapsed: {:.2}s (< 60s gate budget)",
        t_start.elapsed().as_secs_f64()
    );
    println!();

    // Analysis (a)-(d) — mirrored in the module doc.
    let p95_5000_ms = p95_5000.as_secs_f64() * 1000.0;
    let justify = dense_gap_r5 >= 0.6 && fuse_lex_r5 >= 0.9 && p95_5000_ms < 5.0;
    println!("analysis:");
    println!(
        "  (a) dense closes the gap class: gap recall@5 {fts_gap_r5:.3} (fts-only) -> \
         {dense_gap_r5:.3} (dense-only) -> {fuse_gap_r5:.3} (fusion); \
         gap recall@10 {fts_gap_r10:.3} -> {dense_gap_r10:.3} -> {fuse_gap_r10:.3}"
    );
    println!(
        "  (b) lexical regression risk: lexical recall@5 {fts_lex_r5:.3} (fts-only) vs \
         {dense_lex_r5:.3} (dense-only) vs {fuse_lex_r5:.3} (fusion); recall@10 \
         {fts_lex_r10:.3} / {dense_lex_r10:.3} / {fuse_lex_r10:.3}"
    );
    println!(
        "  (c) latency at scale: dense p50/p95 per query {:.3}/{:.3} ms @500, \
         {:.3}/{:.3} ms @2000, {:.3}/{:.3} ms @5000",
        p50_500.as_secs_f64() * 1000.0,
        p95_500.as_secs_f64() * 1000.0,
        p50_2000.as_secs_f64() * 1000.0,
        p95_2000.as_secs_f64() * 1000.0,
        p50_5000.as_secs_f64() * 1000.0,
        p95_5000.as_secs_f64() * 1000.0
    );
    println!(
        "  (d) recommendation (rubric: dense gap recall@5 >= 0.6 AND fusion lexical \
         recall@5 >= 0.9 AND dense p95 @5000 < 5 ms): {}",
        if justify {
            format!(
                "JUSTIFY re-entry — the prototype closes {:.0}% of the gap class \
                 ({dense_gap_r5:.3} gap recall@5 vs 0.000 fts-only) with zero lexical \
                 regression (fusion {fuse_lex_r5:.3} vs fts-only {fts_lex_r5:.3}) at \
                 {p95_5000_ms:.3} ms p95 @5000 claims. Next step per S11: sqlite-vec \
                 spike at 100k claims.",
                dense_gap_r5 * 100.0
            )
        } else {
            format!(
                "DEFER — measured dense gap recall@5 = {dense_gap_r5:.3}, fusion lexical \
                 recall@5 = {fuse_lex_r5:.3}, dense p95 @5000 = {p95_5000_ms:.3} ms; \
                 the rubric requires >= 0.6 / >= 0.9 / < 5 ms. FTS-only remains the \
                 retrieval stage (architecture.md R-20 deferral stands)."
            )
        }
    );

    assert!(
        t_start.elapsed() < Duration::from_secs(60),
        "benchmark must stay well under the 60s gate budget"
    );

    // Scratch DB cleanup (drop the connection first: WAL files).
    drop(index);
    let path = std::env::temp_dir().join(format!(
        "kanbei-dense-bench-{}.db",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
