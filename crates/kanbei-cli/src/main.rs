//! kanbei — a terminal REPL over the kanbei driver.
//!
//! Usage: `kanbei [DIR]`
//!
//! DIR defaults to `$KANBEI_DIR`, then `.` (the session dir). The session dir
//! is also the project-config root: `discover_config_layers` activates the
//! built-in defaults, then `$XDG_CONFIG_HOME/kanbei/init.lua` (or
//! `$HOME/.config/kanbei/init.lua`), then `<DIR>/.kanbei/init.lua`. The
//! provider and approval wiring — engine, base URL, model, protocol, key
//! reference, auto-approval, yolo — comes from the merged config layers
//! through [`CliSettings`] (decision 28), not argv.
//!
//! Bootstrap env surface (all other `KANBEI_*` env was retired by decision 28):
//! - `KANBEI_DIR` — the session/layout root when no positional DIR is given.
//! - `KANBEI_PROVIDER_URL` / `KANBEI_PROVIDER_KEY` — read only as fallbacks
//!   when config does not supply a base URL/key; config wins.
//!
//! `KANBEI_YOLO` and `KANBEI_PROVIDER_MODEL` are intentionally GONE (decision
//! 28): approval policy (`approval.yolo`/`auto_approve`) and the model
//! (`provider.model`) are config fields now, so a repo's config layer cannot
//! be silently overridden by env. `fs_root` is the session dir.
//!
//! The REPL reads one user message per line and drives the resulting wakes
//! to quiescence: the model's final answer is printed to stdout; intermediate
//! tool round-trips are canonical facts (inspect with `/history`). Commands:
//! `/status`, `/history [N]`, `/export DIR`, `/resume` (after a breaker
//! pause), `/exit`.

use std::io::{IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_driver::{Driver, Turn};
use kanbei_modules::PackageManifest;
use kanbei_provider::{
    CompletionRequest, CompletionResponse, FinishReason, KeySource, ProviderConfig,
    ProviderEngine, ProviderError, Usage, WireProtocol,
};
use kanbei_scopes::contrib::{KeyReference, ProviderSettings, SettingsContribution};
use kanbei_session::{
    ApprovalResolver, Session, SessionConfig, SessionSettings, SettingsSource,
};
use kanbei_tools::{ApprovalParked, ToolRegistry};
use kanbei_ui::terminal::{TerminalGuard, TermiosTerminal};
use kanbei_vm::VmConfig;

use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};

const USAGE: &str = "usage: kanbei [DIR]";

/// Bootstrap-only CLI options (decision 28): argv/env no longer carry provider
/// or approval wiring — that lives in the config layers. `dir` is the session
/// layout root and the project-config root.
#[derive(Debug)]
struct Options {
    dir: PathBuf,
}

impl Options {
    fn from_env() -> Self {
        Self {
            dir: std::env::var("KANBEI_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::from_env();
    let mut positional: Option<String> = None;
    for arg in args {
        if arg.starts_with('-') {
            return Err(format!("unknown argument: {arg}"));
        }
        if positional.is_some() {
            return Err(format!("unexpected argument: {arg}"));
        }
        positional = Some(arg.clone());
    }
    if let Some(dir) = positional {
        opts.dir = PathBuf::from(dir);
    }
    Ok(opts)
}

/// The config `provider.fake` engine: replays one scripted response on every
/// call — smoke runs only (real runs set a `provider.base_url`).
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

/// Bootstrap provider config: config `provider` settings override the
/// `KANBEI_PROVIDER_URL` / `KANBEI_PROVIDER_KEY` env fallback (the documented
/// bootstrap exception), so config wins when present. None when neither
/// supplies a base URL — the session then runs storage-only.
fn bootstrap_provider(provider: Option<&ProviderSettings>) -> Option<ProviderConfig> {
    let base_url = provider
        .and_then(|p| p.base_url.clone())
        .or_else(|| std::env::var("KANBEI_PROVIDER_URL").ok())?;
    let key = match provider.and_then(|p| p.key.as_ref()) {
        Some(KeyReference::Env { name }) => KeySource::Env(name.clone()),
        Some(KeyReference::Keychain { service, account }) => KeySource::Keychain {
            service: service.clone(),
            account: account.clone(),
        },
        None => KeySource::Env("KANBEI_PROVIDER_KEY".into()),
    };
    let model = provider
        .and_then(|p| p.model.clone())
        .unwrap_or_else(|| "default".into());
    Some(ProviderConfig {
        provider: "http".into(),
        model,
        base_url,
        key,
        temperature: None,
        max_tokens: None,
        timeout: std::time::Duration::from_secs(60),
    })
}

/// Whether a provider key was explicitly configured: either the config set
/// `provider.key`, or the documented bootstrap env mode is in play (the base
/// URL also came from `KANBEI_PROVIDER_URL`, so the default
/// `KANBEI_PROVIDER_KEY` env is being used deliberately). A keyless endpoint
/// (ollama/vLLM) configured only by `provider.base_url` is NOT explicit — the
/// open-time probe must not force it storage-only.
fn bootstrap_key_is_explicit(provider: Option<&ProviderSettings>) -> bool {
    if provider.and_then(|p| p.key.as_ref()).is_some() {
        return true;
    }
    provider.and_then(|p| p.base_url.as_ref()).is_none()
        && std::env::var("KANBEI_PROVIDER_URL").is_ok()
}

/// Config `provider.protocol` → wire protocol; absent/unknown = the
/// OpenAI-compatible default.
fn parse_protocol(protocol: Option<&str>) -> WireProtocol {
    match protocol {
        Some("anthropic") => WireProtocol::Anthropic,
        _ => WireProtocol::OpenAI,
    }
}

/// The CLI's config→runtime mapping (decision 28): the merged settings drive
/// the provider engine/config, the yolo broker + session id, and the approval
/// resolver. `interactive` is the resolver for the non-auto case (the REPL's
/// stdin prompt or the TUI's cross-thread rendezvous).
struct CliSettings {
    interactive: Option<ApprovalResolver>,
    /// The yolo session identity, minted ONCE per source (F10 idempotency) so
    /// repeated resolves return identical values; the broker is
    /// deterministically rebuilt from it on each resolve (a `Broker` is not
    /// `Clone`, but its grants/templates derive only from the id).
    yolo: std::sync::OnceLock<Id128>,
}

impl SettingsSource for CliSettings {
    fn resolve(&self, settings: &SettingsContribution) -> SessionSettings {
        let provider = settings.provider.as_ref();
        let fake = provider.and_then(|p| p.fake).unwrap_or(false);
        let bootstrap = bootstrap_provider(provider);
        let (provider_engine, provider_config): (
            Option<Box<dyn ProviderEngine>>,
            Option<ProviderConfig>,
        ) = if fake {
            // The scripted one-shot engine for smoke runs.
            (Some(Box::new(RepeatedEngine::fake())), None)
        } else if let Some(config) = bootstrap {
            // F12/F: open-time key availability probe for a real engine — only
            // when a key was explicitly configured. A keyless endpoint must not
            // be probed against the default `KANBEI_PROVIDER_KEY` env. The
            // scripted fake engine needs no key, so it is exempt. On failure the
            // CLI degrades to storage-only (never fails open) and prints an
            // actionable, secret-free line ONCE, not on every layer resolve.
            let protocol = parse_protocol(provider.and_then(|p| p.protocol.as_deref()));
            let build_engine = |config: ProviderConfig| {
                (
                    Some(kanbei_provider::engine_for(&config, protocol)),
                    Some(config),
                )
            };
            if bootstrap_key_is_explicit(provider) {
                if let Err(e) = config.key.probe(&config.provider) {
                    static PROBE_NOTICE: std::sync::Once = std::sync::Once::new();
                    PROBE_NOTICE.call_once(|| {
                        eprintln!(
                            "kanbei: provider key unavailable ({e}); \
                             starting storage-only — the session runs without model calls"
                        );
                    });
                    (None, None)
                } else {
                    build_engine(config)
                }
            } else {
                build_engine(config)
            }
        } else {
            (None, None)
        };

        let approval = settings.approval.as_ref();
        let yolo = approval.and_then(|a| a.yolo).unwrap_or(false);
        let auto = yolo || approval.and_then(|a| a.auto_approve).unwrap_or(false);
        let (broker, session_id) = if yolo {
            let id = *self.yolo.get_or_init(Id128::generate);
            (Some(yolo_broker(id)), Some(id))
        } else {
            (None, None)
        };
        let approval_resolver = if auto {
            Some(Arc::new(|_p: &ApprovalParked| true) as ApprovalResolver)
        } else {
            self.interactive.clone()
        };

        SessionSettings {
            provider_engine,
            provider: provider_config,
            broker,
            approval_resolver,
            session_id,
        }
    }
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

/// The M2 fuel recipe (module activation and host-ABI round-trips exceed the
/// 1M default per call) plus the R-24 bounds: a finite relative epoch (500
/// ticks ~= 5 s, matching `call_timeout`) so a runaway guest is interrupted
/// mid-call, a per-generation wall-clock budget, and the host-import worker
/// ceiling.
fn cli_engine() -> VmConfig {
    VmConfig {
        fuel_per_call: 1u64 << 35,
        epoch_deadline: 500,
        generation_budget: Duration::from_secs(300),
        max_inflight_host_calls: 32,
        ..Default::default()
    }
}

/// YOLO broker: every builtin tool, verb `call`, granted to this launch's
/// session principal with no approvals and no budget. Grants are
/// launch-local by design (the broker never persists); `session_id` is the
/// id this launch adopted (`SessionConfig::session_id`), so kernel-originated
/// calls (principal `session == session_id`) match on fresh and resumed
/// sessions alike.
fn yolo_broker(session_id: Id128) -> Broker {
    let tools = ToolRegistry::builtin().names();
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: tools
                .iter()
                .map(|t| Capability::new(t.clone(), vec!["call".into()]))
                .collect(),
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .expect("yolo policy");
    for tool in tools {
        let mut grant = Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: Capability::new(tool, vec!["call".into()]),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("yolo: full auto-approval".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).expect("yolo grant");
    }
    broker
}

/// Discovers the desired-state config layers for the session dir. A discovery
/// failure (a user/project config file that exists but cannot be read) must
/// not abort startup: surface an actionable line, degrade to built-in-only
/// layers, and hand the reason to `Session::open` so it lands as a canonical
/// `safe_mode_activated` fact (F14). Safe mode remains the activation-failure
/// path (a syntactically broken file is read, then fails to activate).
fn discover_config_layers_or_default(dir: &Path) -> (Vec<PackageManifest>, Option<String>) {
    match kanbei_session::discover_config_layers(dir) {
        Ok(layers) => (layers, None),
        Err(e) => {
            let reason = e.to_string();
            eprintln!("kanbei: {reason}; falling back to built-in config defaults");
            (vec![kanbei_session::builtin_config_manifest()], Some(reason))
        }
    }
}

/// Piped-stdin path: the plain line REPL.
fn run_repl(opts: Options) {
    let interactive: ApprovalResolver = Arc::new(interactive_approve);
    let (config_layers, config_discovery_error) = discover_config_layers_or_default(&opts.dir);
    let session = match Session::open(SessionConfig {
        dir: opts.dir.clone(),
        stream: "cli".into(),
        engine: Some(cli_engine()),
        fs_root: opts.dir.clone(),
        config_layers,
        config_discovery_error,
        settings: Some(Arc::new(CliSettings {
            interactive: Some(interactive),
            yolo: Default::default(),
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

// ---------- full-screen TUI (TTY path) ----------

/// Worker→main events: only the interactive approval rendezvous crosses back —
/// the worker owns the session and does all rendering (decision 13).
enum Evt {
    /// An approval-gated intent parked during a turn; the UI decides it (y/n)
    /// and replies on `reply`.
    Approval(ApprovalReq),
    /// The kernel's `quit` binding won: the process should shut down.
    Quit,
}

/// main→worker commands.
enum Cmd {
    /// Feed raw terminal bytes through the kernel UI boundary.
    Input(Vec<u8>),
    /// Shut down (the worker closes the session and returns).
    Quit,
}

/// One approval request the UI must decide. `reply` carries the decision back
/// to the worker's resolver (which blocks until answered).
struct ApprovalReq {
    reply: mpsc::Sender<bool>,
}

fn run_tui(opts: Options) -> i32 {
    // main ⇄ worker channels. The worker owns the driver + session (decision
    // 13: single-owner-at-a-time) and drives turns to quiescence; the main
    // thread reads terminal bytes and forwards them, so the approval
    // rendezvous still works while the worker blocks in a turn.
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<Evt>();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Approval seam: the resolver does a cross-thread rendezvous (the worker
    // blocks until the UI answers y/n). The config settings decide
    // auto-approval; the rendezvous is the interactive fallback.
    let approval_tx = evt_tx.clone();
    let interactive: ApprovalResolver = Arc::new(move |_p: &ApprovalParked| {
        let (reply_tx, reply_rx) = mpsc::channel::<bool>();
        let _ = approval_tx.send(Evt::Approval(ApprovalReq { reply: reply_tx }));
        reply_rx.recv().unwrap_or(false)
    });
    let cancel_cfg = cancel_flag.clone();
    let (config_layers, config_discovery_error) = discover_config_layers_or_default(&opts.dir);
    let cfg = SessionConfig {
        dir: opts.dir.clone(),
        stream: "cli".into(),
        engine: Some(cli_engine()),
        fs_root: opts.dir.clone(),
        config_layers,
        config_discovery_error,
        settings: Some(Arc::new(CliSettings {
            interactive: Some(interactive),
            yolo: Default::default(),
        })),
        cancel_flag: Some(cancel_cfg),
        ..Default::default()
    };

    let mut session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kanbei: session open failed: {e}");
            return 2;
        }
    };
    // The shipped binary drives the UI host (decision 31): the built-in shell
    // module owns layout, the kernel presents its frames. A missing guest
    // leaves the host unbound and the CLI renders nothing (fail-loud), so the
    // module engine is a hard prerequisite of the TUI path.
    if session.modules().is_none() {
        eprintln!("kanbei: module engine unavailable: build the guest wasm");
        return 2;
    }
    if let Err(e) = session.activate_builtin_ui() {
        eprintln!("kanbei: built-in UI activation failed: {e}");
        return 2;
    }

    // Terminal lifecycle: raw mode through the kernel boundary (restored by
    // the guard on every exit path), alternate screen through crossterm.
    let Some((mut raw_term, present_term)) = open_terminal() else {
        eprintln!("kanbei: could not open the terminal");
        return 2;
    };
    let guard = match TerminalGuard::new(&mut raw_term) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("kanbei: raw mode failed: {e}");
            return 2;
        }
    };
    if execute!(std::io::stdout(), EnterAlternateScreen).is_err() {
        eprintln!("kanbei: alternate screen failed");
        return 2;
    }

    // Worker: renders/presents after every command and drives a submitted
    // turn. Rendering is event-driven (decision 22): only a command or a
    // completed turn repaints; a scroll/focus repaint never re-enters Wasm.
    let quit_tx = evt_tx.clone();
    let worker = std::thread::spawn(move || {
        let mut driver = Driver::new(session);
        let mut term = present_term;
        present(&mut driver, &mut term);
        // A `Cmd::Quit` (or a dropped sender) ends the loop; the session closes
        // below.
        while let Ok(Cmd::Input(bytes)) = cmd_rx.recv() {
            match driver.session_mut().ui_handle_input(&bytes) {
                Ok(outcome) => {
                    if outcome.quit {
                        let _ = quit_tx.send(Evt::Quit);
                        break;
                    }
                    if outcome.submitted
                        && let Err(e) = driver.drive_to_quiescence()
                    {
                        eprintln!("kanbei: turn failed: {e}");
                    }
                }
                Err(e) => eprintln!("kanbei: ui input failed: {e}"),
            }
            present(&mut driver, &mut term);
        }
        let _ = driver.into_session().close();
    });

    // Reader thread: raw bytes in, so the kernel's decoder owns escape/paste
    // handling (decision 31). Blocking reads are fine — the process exits
    // without joining it.
    let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if input_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });

    let mut pending: Option<mpsc::Sender<bool>> = None;
    let mut quit = false;
    loop {
        while let Ok(evt) = evt_rx.try_recv() {
            match evt {
                Evt::Approval(req) => pending = Some(req.reply),
                Evt::Quit => quit = true,
            }
        }
        if quit {
            break;
        }
        match input_rx.recv_timeout(Duration::from_millis(16)) {
            Ok(bytes) => {
                // Ctrl-C interrupts an in-flight model call at the stream
                // boundary (decision 13). Every other key, including Ctrl-Q,
                // dispatches through the kernel keymap (decision 29).
                if bytes.contains(&0x03) {
                    cancel_flag.store(true, Ordering::SeqCst);
                }
                if let Some(reply) = pending.take() {
                    match approval_decision(&bytes) {
                        Some(decision) => {
                            reply.send(decision).ok();
                        }
                        None => pending = Some(reply),
                    }
                    continue;
                }
                if cmd_tx.send(Cmd::Input(bytes)).is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // If an approval is pending, deny it to unblock the worker's resolver
    // before joining (a blocked resolver would hang the join).
    if let Some(reply) = pending {
        reply.send(false).ok();
    }
    let _ = cmd_tx.send(Cmd::Quit);
    let _ = worker.join();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    drop(guard);
    0
}

/// Present the session's canonical frame through the kernel terminal boundary.
fn present(driver: &mut Driver, term: &mut TermiosTerminal) {
    if let Err(e) = driver.session_mut().ui_present(term) {
        eprintln!("kanbei: present failed: {e}");
    }
}

/// The y/n decision in one input burst, if any (approval keys only).
fn approval_decision(bytes: &[u8]) -> Option<bool> {
    if bytes.iter().any(|b| matches!(b, b'y' | b'Y')) {
        return Some(true);
    }
    if bytes.iter().any(|b| matches!(b, b'n' | b'N' | 0x03)) {
        return Some(false);
    }
    None
}

/// Two terminal handles over the process's tty: stdin (raw mode, bytes) and
/// stdout (frames). Each is an owned duplicated fd, so the kernel boundary
/// stays fd-scoped.
fn open_terminal() -> Option<(TermiosTerminal, TermiosTerminal)> {
    let stdin = std::io::stdin();
    let raw = TermiosTerminal::open(stdin.as_fd().try_clone_to_owned().ok()?).ok()?;
    let stdout = std::io::stdout();
    let present = TermiosTerminal::open(stdout.as_fd().try_clone_to_owned().ok()?).ok()?;
    Some((raw, present))
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
    let code = if std::io::stdin().is_terminal() {
        run_tui(opts)
    } else {
        run_repl(opts);
        0
    };
    exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_positional_dir() {
        let opts = parse_args(&["/tmp/x".into()]).unwrap();
        assert_eq!(opts.dir, PathBuf::from("/tmp/x"));
    }

    /// Decision 28: the provider/approval flags are gone — argv is
    /// bootstrap-only (`DIR`).
    #[test]
    fn parse_rejects_removed_flags() {
        for flag in [
            "--fake",
            "--auto-approve",
            "--yolo",
            "--model",
            "--model=m",
        ] {
            assert!(
                parse_args(&[flag.to_string()]).is_err(),
                "removed flag still accepted: {flag}"
            );
        }
    }

    #[test]
    fn parse_rejects_unknown_and_dangling() {
        assert!(parse_args(&["--nope".into()]).is_err());
        assert!(parse_args(&["a".to_string(), "b".to_string()]).is_err());
    }

    #[test]
    fn yolo_broker_grants_every_builtin_tool_without_approval() {
        let id = Id128::generate();
        let broker = yolo_broker(id);
        for name in ToolRegistry::builtin().names() {
            let eff = broker
                .check(
                    &Principal {
                        session: id,
                        generation: 0,
                        run: Some(0),
                    },
                    &Capability::new(name.clone(), vec!["call".into()]),
                    1,
                )
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!eff.requires_approval, "{name}");
        }
    }

    /// The settings source maps a config `fake` provider to the scripted engine
    /// and `yolo` to a broker + session id.
    #[test]
    fn cli_settings_resolves_config_driven_wiring() {
        // SAFETY: unique env name, only read by this test's settings source.
        unsafe { std::env::set_var("KANBEI_TEST_SETTINGS_PRESENT_KEY", "k") };
        let interactive: ApprovalResolver = Arc::new(|_| false);
        let source = CliSettings {
            interactive: Some(interactive),
            yolo: Default::default(),
        };
        let fake: SettingsContribution =
            serde_json::from_str(r#"{"provider":{"fake":true}}"#).unwrap();
        let resolved = source.resolve(&fake);
        assert_eq!(resolved.provider_engine.unwrap().identity(), "fake");
        assert!(resolved.provider.is_none());

        let http: SettingsContribution = serde_json::from_str(
            r#"{"provider":{"base_url":"https://x","model":"m","protocol":"anthropic","key":{"Env":{"name":"KANBEI_TEST_SETTINGS_PRESENT_KEY"}}}}"#,
        )
        .unwrap();
        let resolved = source.resolve(&http);
        assert_eq!(resolved.provider_engine.unwrap().identity(), "http");
        assert_eq!(resolved.provider.unwrap().model, "m");

        let yolo: SettingsContribution = serde_json::from_str(r#"{"approval":{"yolo":true}}"#).unwrap();
        let resolved = source.resolve(&yolo);
        assert!(resolved.broker.is_some(), "yolo wires a broker");
        assert!(resolved.session_id.is_some(), "yolo wires a session id");
        assert!(
            resolved.approval_resolver.is_some(),
            "yolo implies auto-approval"
        );

        let plain: SettingsContribution = serde_json::from_str(r#"{"approval":{"auto_approve":false}}"#).unwrap();
        let resolved = source.resolve(&plain);
        assert!(
            resolved.approval_resolver.is_some(),
            "the interactive resolver remains the fallback"
        );
        assert!(resolved.broker.is_none(), "no yolo → no broker override");
    }

    /// F10: yolo's session id (and therefore its broker) is minted once per
    /// source, so repeated resolves agree.
    #[test]
    fn cli_settings_yolo_identity_is_idempotent() {
        let source = CliSettings {
            interactive: None,
            yolo: Default::default(),
        };
        let yolo: SettingsContribution = serde_json::from_str(r#"{"approval":{"yolo":true}}"#).unwrap();
        let first = source.resolve(&yolo);
        let second = source.resolve(&yolo);
        assert_eq!(
            first.session_id, second.session_id,
            "the yolo session id is cached"
        );
        assert_eq!(
            first.broker.as_ref().map(|b| b.grants.len()),
            second.broker.as_ref().map(|b| b.grants.len()),
            "the broker is rebuilt deterministically from the cached id"
        );
    }

    /// F12: a present-but-unresolvable key probe degrades to storage-only (no
    /// engine) instead of failing open.
    #[test]
    fn cli_settings_probe_failure_is_storage_only() {
        let source = CliSettings {
            interactive: None,
            yolo: Default::default(),
        };
        let http: SettingsContribution = serde_json::from_str(
            r#"{"provider":{"base_url":"https://x","model":"m","key":{"Env":{"name":"KANBEI_TEST_SETTINGS_ABSENT_KEY"}}}}"#,
        )
        .unwrap();
        let resolved = source.resolve(&http);
        assert!(
            resolved.provider_engine.is_none(),
            "unavailable key → no provider engine"
        );
        assert!(resolved.provider.is_none());
    }

    /// F: a keyless endpoint configured only by `provider.base_url` (ollama/
    /// vLLM) must NOT be probed against the default `KANBEI_PROVIDER_KEY` env —
    /// the engine stays wired even though no key is available.
    #[test]
    fn cli_keyless_config_base_url_keeps_engine() {
        let source = CliSettings {
            interactive: None,
            yolo: Default::default(),
        };
        let keyless: SettingsContribution =
            serde_json::from_str(r#"{"provider":{"base_url":"http://localhost:11434/v1","model":"llama"}}"#)
                .unwrap();
        let resolved = source.resolve(&keyless);
        assert!(
            resolved.provider_engine.is_some(),
            "a keyless endpoint keeps its provider engine"
        );
        assert!(resolved.provider.is_some(), "the config is retained");
    }
}
