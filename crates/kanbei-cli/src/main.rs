//! kanbei — a terminal REPL over the kanbei driver.
//!
//! Usage: `kanbei [DIR] [--model M] [--fake] [--auto-approve]`
//!
//! DIR defaults to `$KANBEI_DIR`, then `.` (the session dir). Provider:
//! `--fake` (a scripted one-shot engine for smoke runs) or
//! `KANBEI_PROVIDER_URL` / `KANBEI_PROVIDER_KEY` / `KANBEI_PROVIDER_MODEL`
//! (an OpenAI-compatible chat-completions endpoint; `--model` overrides the
//! env). `fs_root` is the session dir.
//!
//! The REPL reads one user message per line and drives the resulting wakes
//! to quiescence: the model's final answer is printed to stdout; intermediate
//! tool round-trips are canonical facts (inspect with `/history`). Commands:
//! `/status`, `/history [N]`, `/export DIR`, `/resume` (after a breaker
//! pause), `/exit`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use kanbei_driver::{Driver, Turn};
use kanbei_provider::{
    CompletionRequest, CompletionResponse, FinishReason, HttpEngine, KeySource, ProviderConfig,
    ProviderEngine, ProviderError, Usage,
};
use kanbei_session::{Session, SessionConfig};
use kanbei_tools::ApprovalParked;
use kanbei_vm::VmConfig;

const USAGE: &str = "usage: kanbei [DIR] [--model M] [--fake] [--auto-approve]";

#[derive(Debug)]
struct Options {
    dir: PathBuf,
    model: Option<String>,
    fake: bool,
    auto_approve: bool,
}

impl Options {
    fn from_env() -> Self {
        Self {
            dir: std::env::var("KANBEI_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(".")),
            model: std::env::var("KANBEI_PROVIDER_MODEL").ok(),
            fake: false,
            auto_approve: false,
        }
    }
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::from_env();
    let mut positional: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fake" => opts.fake = true,
            "--auto-approve" => opts.auto_approve = true,
            "--model" => {
                i += 1;
                opts.model = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "missing value for --model".to_string())?,
                );
            }
            _ if args[i].starts_with("--model=") => {
                opts.model = Some(args[i][8..].to_string());
            }
            s if !s.starts_with('-') => {
                if positional.is_some() {
                    return Err(format!("unexpected argument: {s}"));
                }
                positional = Some(s.to_string());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }
    if let Some(dir) = positional {
        opts.dir = PathBuf::from(dir);
    }
    Ok(opts)
}

/// `--fake` engine: replays one scripted response on every call — smoke
/// runs only (real runs wire KANBEI_PROVIDER_*).
struct RepeatedEngine {
    cfg: ProviderConfig,
    response: CompletionResponse,
}

impl RepeatedEngine {
    fn fake() -> Self {
        Self {
            cfg: ProviderConfig {
                provider: "fake".into(),
                model: "repl".into(),
                base_url: "http://localhost:0/v1".into(),
                key: KeySource::Env("KANBEI_PROVIDER_KEY".into()),
                temperature: None,
                max_tokens: None,
                timeout: std::time::Duration::from_secs(5),
            },
            response: CompletionResponse {
                content: Some("kanbei ready (fake provider — set KANBEI_PROVIDER_URL/KEY for real completions)".into()),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: Usage { input_tokens: 0, output_tokens: 0 },
                discontinuity: None,
                opaque_artifacts: None,
            },
        }
    }
}

impl ProviderEngine for RepeatedEngine {
    fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(self.response.clone())
    }
    fn identity(&self) -> &str {
        &self.cfg.provider
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn http_config(opts: &Options) -> Result<ProviderConfig, String> {
    let base_url = std::env::var("KANBEI_PROVIDER_URL")
        .map_err(|_| "KANBEI_PROVIDER_URL is required (or use --fake)".to_string())?;
    Ok(ProviderConfig {
        provider: "http".into(),
        model: opts
            .model
            .clone()
            .unwrap_or_else(|| "default".into()),
        base_url,
        key: KeySource::Env("KANBEI_PROVIDER_KEY".into()),
        temperature: None,
        max_tokens: None,
        timeout: std::time::Duration::from_secs(60),
    })
}

/// The interactive approval stand-in for the driver's approval resolver:
/// presents the parked intent's committed action + arguments and asks.
fn interactive_approve(p: &ApprovalParked) -> bool {
    eprint!(
        "\napproval required: {} args={}\napprove? [y/N] ",
        p.approval.action, p.approval.args
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

fn print_turn(turn: &Turn) {
    if let Some(answer) = &turn.answer {
        println!("{answer}");
    }
    if turn.answer.is_none() {
        eprintln!(
            "no answer after {} run(s) (last outcome: {:?}) — see the canonical record",
            turn.runs, turn.last_outcome
        );
    }
}

fn repl(driver: &mut Driver) {
    let s = driver.session();
    let identity = s
        .provider_engine()
        .map(|e| e.identity().to_string())
        .unwrap_or_else(|| "(none)".into());
    eprintln!(
        "kanbei: session {} via {identity} — /status /history [N] /export DIR /resume /exit",
        s.session_id()
    );
    loop {
        eprint!("\nyou> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "/exit" | "/quit" => break,
            "/status" => {
                let s = driver.session();
                eprintln!(
                    "session {}  next_seq {}  config {}  pending approvals {}",
                    s.session_id(),
                    s.next_seq(),
                    s.config_digest()
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| "(none)".into()),
                    s.pending_approvals().len()
                );
                continue;
            }
            "/resume" => {
                match driver.resume() {
                    Ok(turn) => print_turn(&turn),
                    Err(e) => eprintln!("resume failed: {e}"),
                }
                continue;
            }
            _ => {}
        }
        if line.starts_with("/history") {
            let n: u64 = line
                .strip_prefix("/history")
                .and_then(|rest| rest.trim().parse().ok())
                .unwrap_or(10);
            let s = driver.session();
            let last = s.next_seq();
            let start = last.saturating_sub(n);
            for seq in start.max(1)..last {
                match s.envelope_at(seq) {
                    Ok(env) => {
                        let mut text =
                            serde_json::to_string(&env.payload).unwrap_or_default();
                        if text.len() > 160 {
                            text.truncate(160);
                            text.push('…');
                        }
                        eprintln!("{seq} {} {text}", env.kind);
                    }
                    Err(e) => eprintln!("{seq} (unavailable: {e})"),
                }
            }
            continue;
        }
        if let Some(dir) = line.strip_prefix("/export") {
            let dir = dir.trim();
            if dir.is_empty() {
                eprintln!("usage: /export DIR");
                continue;
            }
            match driver.session_mut().export_bundle(Path::new(dir)) {
                Ok(report) => eprintln!(
                    "exported: frames {} envelopes {} objects {} verified {}",
                    report.frames, report.envelopes, report.objects, report.verified
                ),
                Err(e) => eprintln!("export failed: {e}"),
            }
            continue;
        }
        match driver.user_turn(line) {
            Ok(turn) => print_turn(&turn),
            Err(e) => eprintln!("turn failed: {e}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("kanbei: {e}");
            eprintln!("{USAGE}");
            exit(2);
        }
    };
    let engine: Box<dyn ProviderEngine> = if opts.fake {
        Box::new(RepeatedEngine::fake())
    } else {
        match http_config(&opts) {
            Ok(cfg) => Box::new(HttpEngine::new(cfg)),
            Err(e) => {
                eprintln!("kanbei: {e}");
                exit(2);
            }
        }
    };
    let session = match Session::open(SessionConfig {
        dir: opts.dir.clone(),
        stream: "cli".into(),
        // The M2 fuel recipe (same as the workbench): module activation and
        // the host-ABI round-trips exceed the 1M default per call.
        engine: Some(VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX / 2,
            ..Default::default()
        }),
        provider_engine: Some(engine),
        fs_root: opts.dir.clone(),
        approval_resolver: Some(Arc::new(if opts.auto_approve {
            |_p| true
        } else {
            interactive_approve
        })),
        ..Default::default()
    }) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("kanbei: session open failed: {e}");
            exit(2);
        }
    };
    let mut driver = Driver::new(session);
    repl(&mut driver);
    if let Err(e) = driver.into_session().close() {
        eprintln!("kanbei: session close failed: {e}");
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags_and_positional() {
        let args: Vec<String> = ["--model", "m1", "--fake", "--auto-approve", "/tmp/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = parse_args(&args).unwrap();
        assert!(opts.fake && opts.auto_approve);
        assert_eq!(opts.model.as_deref(), Some("m1"));
        assert_eq!(opts.dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn parse_equals_form() {
        let args: Vec<String> = ["--model=m2", "/tmp/y"].iter().map(|s| s.to_string()).collect();
        let opts = parse_args(&args).unwrap();
        assert_eq!(opts.model.as_deref(), Some("m2"));
        assert_eq!(opts.dir, PathBuf::from("/tmp/y"));
    }

    #[test]
    fn parse_rejects_unknown_and_dangling() {
        assert!(parse_args(&["--nope".into()]).is_err());
        assert!(parse_args(&["--model".into()]).is_err());
        assert!(parse_args(&["a".to_string(), "b".to_string()]).is_err());
    }
}
