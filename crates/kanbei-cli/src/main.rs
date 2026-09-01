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

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use kanbei_core::envelope::Envelope;
use kanbei_driver::{Driver, Turn};
use kanbei_provider::{
    CompletionRequest, CompletionResponse, FinishReason, HttpEngine, KeySource, ProviderConfig,
    ProviderEngine, ProviderError, Usage,
};
use kanbei_session::{Session, SessionConfig, SessionError};
use kanbei_tools::ApprovalParked;
use kanbei_ui::{
    build_viewport, key_to_input, resolve_style, total_rows, transcript_paragraph,
    ConversationState, InputEvent, Row, StyledRow, Theme,
};
use kanbei_vm::VmConfig;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseEvent};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

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

/// Build the provider engine for the run (`--fake` or the wire endpoint).
fn build_engine(opts: &Options) -> Result<Box<dyn ProviderEngine>, String> {
    if opts.fake {
        Ok(Box::new(RepeatedEngine::fake()))
    } else {
        let cfg = http_config(opts)?;
        Ok(Box::new(HttpEngine::new(cfg)))
    }
}

/// The M2 fuel recipe (module activation and host-ABI round-trips exceed the
/// 1M default per call).
fn cli_engine() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX / 2,
        ..Default::default()
    }
}

/// Piped-stdin path: the plain line REPL.
fn run_repl(opts: Options, engine: Box<dyn ProviderEngine>) {
    let session = match Session::open(SessionConfig {
        dir: opts.dir.clone(),
        stream: "cli".into(),
        engine: Some(cli_engine()),
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

// ---------- full-screen TUI (TTY path) ----------

/// Worker→main events.
enum Evt {
    /// A committed envelope (replay or live) to fold into the transcript.
    Envelope(Envelope),
    /// The canonical replay is complete; resolve any leftover active turn.
    ReplayDone,
    /// An approval-gated intent parked during a turn; the UI decides it
    /// (y/n) and replies on `reply`.
    Approval(ApprovalReq),
    /// A turn finished (or failed) — carries the driver's result.
    TurnDone(Result<Turn, SessionError>),
}

/// main→worker commands.
enum Cmd {
    /// Run a user turn (commit the message + drive to quiescence).
    Submit(String),
    /// Shut down (the worker closes the session and returns).
    Quit,
}

/// One approval request the UI must decide. `reply` carries the decision back
/// to the worker's resolver (which blocks until answered).
struct ApprovalReq {
    action: String,
    args: String,
    reply: mpsc::Sender<bool>,
}

/// TUI focus: the input line (default) or a transcript turn (j/k/Enter).
#[derive(Debug, Clone, Copy)]
enum Focus {
    Input,
    Transcript { sel: usize },
}

impl Focus {
    fn is_browse(&self) -> bool {
        matches!(self, Focus::Transcript { .. })
    }
}

/// The mutable UI state (main thread).
struct Ui {
    conv: ConversationState,
    expanded: HashSet<String>,
    input: String,
    cursor: usize,
    focus: Focus,
    scroll_top: usize,
    pinned: bool,
    model: String,
    pending: Option<ApprovalReq>,
    active: bool,
    status: String,
    quit: bool,
    repaint: bool,
}

impl Ui {
    fn new(model: String) -> Self {
        Self {
            conv: ConversationState::new(),
            expanded: HashSet::new(),
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            scroll_top: 0,
            pinned: true,
            model,
            pending: None,
            active: false,
            status: "idle".into(),
            quit: false,
            repaint: false,
        }
    }
}

fn run_tui(opts: Options, engine: Box<dyn ProviderEngine>) -> i32 {
    let model = opts
        .model
        .unwrap_or_else(|| if opts.fake { "fake".into() } else { "default".into() });

    // main ⇄ worker channels. The worker owns the driver + session and drives
    // turns to quiescence; the main thread renders and routes input, so the
    // UI stays responsive while a turn runs (R-27 UI boundary).
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<Evt>();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Observer + approval seams: commit_listener fires on the committing
    // (worker) thread per resolved envelope; the resolver does a cross-thread
    // rendezvous (the worker blocks until the UI answers y/n).
    let commit_tx = evt_tx.clone();
    let approval_tx = evt_tx.clone();
    let auto = opts.auto_approve;
    let cancel_cfg = cancel_flag.clone();
    let cfg = SessionConfig {
        dir: opts.dir.clone(),
        stream: "cli".into(),
        engine: Some(cli_engine()),
        provider_engine: Some(engine),
        fs_root: opts.dir.clone(),
        approval_resolver: Some(Arc::new(move |p: &ApprovalParked| {
            if auto {
                return true;
            }
            let (reply_tx, reply_rx) = mpsc::channel::<bool>();
            let _ = approval_tx.send(Evt::Approval(ApprovalReq {
                action: p.approval.action.clone(),
                args: p.approval.args.to_string(),
                reply: reply_tx,
            }));
            reply_rx.recv().unwrap_or(false)
        })),
        commit_listener: Some(Arc::new(move |env: &Envelope| {
            let _ = commit_tx.send(Evt::Envelope(env.clone()));
        })),
        cancel_flag: Some(cancel_cfg),
        ..Default::default()
    };

    let session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kanbei: session open failed: {e}");
            return 2;
        }
    };

    // Worker thread: replay the canonical log (launch = resume, R-19), then
    // drive turns. On Quit it closes the session (it owns it).
    let worker = std::thread::spawn(move || {
        let mut driver = Driver::new(session);
        let _ = driver
            .session()
            .replay_envelopes(0, |env| {
                let _ = evt_tx.send(Evt::Envelope(env.clone()));
            });
        let _ = evt_tx.send(Evt::ReplayDone);
        loop {
            match cmd_rx.recv() {
                Ok(Cmd::Submit(text)) => {
                    let res = driver.user_turn(&text);
                    let _ = evt_tx.send(Evt::TurnDone(res));
                }
                Ok(Cmd::Quit) => break,
                Err(_) => break, // main dropped cmd_tx (shut down)
            }
        }
        let _ = driver.into_session().close();
    });

    let (code, mut ui) = run_tui_loop(&evt_rx, &cmd_tx, &cancel_flag, model, auto);

    // If an approval is pending, deny it to unblock the worker's resolver
    // before joining (a blocked resolver would hang the join).
    if let Some(p) = ui.pending.take() {
        p.reply.send(false).ok();
    }
    // Shut down: tell the worker to quit, join it (it closes the session).
    let _ = cmd_tx.send(Cmd::Quit);
    let _ = worker.join();
    code
}

/// The main render + input loop (crossterm events and worker events
/// interleaved on a short poll; ~60 fps).
fn run_tui_loop(
    evt_rx: &mpsc::Receiver<Evt>,
    cmd_tx: &mpsc::Sender<Cmd>,
    cancel_flag: &Arc<AtomicBool>,
    model: String,
    auto: bool,
) -> (i32, Ui) {
    let theme = Theme::default_theme();
    let mut ui = Ui::new(model);
    if auto {
        ui.status = "idle (auto-approve)".into();
    }

    // Terminal lifecycle (R-27): raw mode + alternate screen + mouse capture
    // for the session; restored on every exit path below.
    if terminal::enable_raw_mode().is_err() {
        eprintln!("kanbei: could not enable raw mode");
        return (2, ui);
    }
    if execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture).is_err() {
        let _ = terminal::disable_raw_mode();
        return (2, ui);
    }
    // No term.clear() here: the fresh alternate screen is blank on entry, the
    // first draw covers the full area, and Terminal::clear issues a blocking
    // cursor-position query (\x1b[6n) that a pty never answers.
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = Terminal::new(backend).expect("terminal backend");

    // Mouse hit-testing uses the previous frame's layout (rendered rows).
    let mut last_hit: Vec<usize> = Vec::new();
    let mut last_turn_of_line: Vec<usize> = Vec::new();
    let mut last_transcript: Rect = Rect::new(0, 0, 0, 0);

    let code = loop {
        // 1. Drain worker events (envelopes, approvals, turn results, replay).
        while let Ok(evt) = evt_rx.try_recv() {
            handle_evt(&mut ui, evt);
        }

        // 2. Poll the terminal (short timeout so events interleave with
        //    input). Read in a batch: event::read() blocks on an empty queue,
        //    so it runs only while a poll confirms an event is pending.
        if event::poll(Duration::from_millis(16)).unwrap_or(false) {
            loop {
                let Ok(ev) = event::read() else {
                    break;
                };
                match ev {
                    Event::Key(k) => {
                        if let Some(input) = key_to_input(&k) {
                            if let Some(cmd) = handle_input(&mut ui, input, cancel_flag) {
                                let _ = cmd_tx.send(cmd);
                            }
                        }
                    }
                    Event::Mouse(m) => {
                        handle_mouse(&mut ui, &m, &last_hit, &last_turn_of_line, last_transcript);
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO).unwrap_or(false) {
                    break;
                }
            }
        }

        // 3. Compute the transcript viewport and render.
        let Ok(size) = term.size() else {
            break 0;
        };
        let area = Rect::new(0, 0, size.width, size.height);
        // Ctrl-L: force a full repaint. resize() clears the screen and resets
        // the diff baseline so the next draw re-emits everything; unlike
        // clear(), it issues no blocking cursor-position query.
        if ui.repaint {
            ui.repaint = false;
            if term.resize(area).is_err() {
                break 0;
            }
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        let transcript = chunks[0];
        let input_area = chunks[1];
        let status_area = chunks[2];
        let width = area.width as usize;
        let trows = ui.conv.transcript(&ui.expanded);
        let rows: Vec<Row> = trows
            .iter()
            .map(|t| Row {
                text: t.text.clone(),
                style: t.style.clone(),
            })
            .collect();
        let turn_of_line: Vec<usize> = trows.iter().map(|t| t.turn).collect();
        let total = total_rows(&rows, width);
        let view_h = transcript.height as usize;
        let max_top = total.saturating_sub(view_h);
        let top = if ui.pinned { max_top } else { ui.scroll_top.min(max_top) };
        if !ui.pinned && top == max_top {
            ui.pinned = true;
        }
        let (styled, hit) = build_viewport(&rows, top, view_h, width);

        if term
            .draw(|f| draw(f, &ui, &styled, transcript, input_area, status_area, &theme))
            .is_err()
        {
            break 0; // terminal gone
        }

        last_hit = hit;
        last_turn_of_line = turn_of_line;
        last_transcript = transcript;

        if ui.quit {
            break 0;
        }
    };

    // Restore the terminal on every path.
    let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    (code, ui)
}

/// Route one worker event into the UI state.
fn handle_evt(ui: &mut Ui, evt: Evt) {
    match evt {
        Evt::Envelope(env) => ui.conv.apply(&env),
        Evt::ReplayDone => ui.conv.finish_replay(),
        Evt::Approval(req) => {
            ui.pending = Some(req);
            ui.status = "awaiting approval".into();
        }
        Evt::TurnDone(result) => {
            ui.active = false;
            // The terminal run_outcome is already committed (apply recorded
            // it); finalize flips the turn's state from that recorded outcome.
            ui.conv.finalize_turn(None);
            ui.pinned = true;
            ui.focus = Focus::Input;
            match result {
                Ok(turn) => ui.status = if turn.answer.is_some() {
                    "idle".into()
                } else {
                    format!("no answer · {} run(s)", turn.runs)
                },
                Err(e) => ui.status = format!("turn failed: {e}"),
            }
        }
    }
}

/// Route one sanitized input event. Returns the command to send to the worker
/// (submit / resume) if the input triggered a turn.
fn handle_input(
    ui: &mut Ui,
    input: InputEvent,
    cancel_flag: &Arc<AtomicBool>,
) -> Option<Cmd> {
    // Approvals take priority: y/n (Ctrl-C denies).
    if let Some(p) = &ui.pending {
        return match input {
            InputEvent::Char('y' | 'Y') => {
                p.reply.send(true).ok();
                ui.pending = None;
                ui.status = "approved".into();
                None
            }
            InputEvent::Char('n' | 'N') | InputEvent::CtrlC => {
                p.reply.send(false).ok();
                ui.pending = None;
                ui.status = "denied".into();
                None
            }
            _ => None,
        };
    }
    match ui.focus {
        Focus::Input => match input {
            InputEvent::Char(c) => {
                let b = char_offset(&ui.input, ui.cursor);
                ui.input.insert(b, c);
                ui.cursor += 1;
                None
            }
            InputEvent::Backspace => {
                if ui.cursor == 0 {
                    return None;
                }
                let prev = char_offset(&ui.input, ui.cursor - 1);
                let cur = char_offset(&ui.input, ui.cursor);
                ui.input.replace_range(prev..cur, "");
                ui.cursor -= 1;
                None
            }
            InputEvent::Delete => {
                let cur = char_offset(&ui.input, ui.cursor);
                if cur >= ui.input.len() {
                    return None;
                }
                let next = char_offset(&ui.input, ui.cursor + 1);
                ui.input.replace_range(cur..next, "");
                None
            }
            InputEvent::ArrowLeft => {
                ui.cursor = ui.cursor.saturating_sub(1);
                None
            }
            InputEvent::ArrowRight => {
                if ui.cursor < ui.input.chars().count() {
                    ui.cursor += 1;
                }
                None
            }
            InputEvent::Home => {
                ui.cursor = 0;
                None
            }
            InputEvent::End => {
                ui.cursor = ui.input.chars().count();
                None
            }
            InputEvent::Enter => {
                let text = ui.input.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                ui.active = true;
                ui.input.clear();
                ui.cursor = 0;
                ui.pinned = true;
                ui.status = "running…".into();
                Some(Cmd::Submit(text))
            }
            InputEvent::CtrlC => {
                if ui.active {
                    cancel_flag.store(true, Ordering::SeqCst);
                    ui.status = "cancelling…".into();
                } else {
                    ui.input.clear();
                    ui.cursor = 0;
                }
                None
            }
            InputEvent::CtrlQ => {
                // Quit cancels the active run first (the worker owns the close).
                if ui.active {
                    cancel_flag.store(true, Ordering::SeqCst);
                }
                ui.quit = true;
                None
            }
            InputEvent::CtrlL => {
                ui.repaint = true;
                None
            }
            InputEvent::Escape => {
                let sel = ui.conv.turns.len().saturating_sub(1);
                ui.focus = Focus::Transcript { sel };
                None
            }
            InputEvent::ArrowUp => {
                scroll(ui, 1, false);
                None
            }
            InputEvent::ArrowDown => {
                scroll(ui, 1, true);
                None
            }
            InputEvent::PageUp => {
                scroll(ui, 10, false);
                None
            }
            InputEvent::PageDown => {
                scroll(ui, 10, true);
                None
            }
            _ => None,
        },
        Focus::Transcript { sel } => match input {
            InputEvent::ArrowUp | InputEvent::Char('k') => {
                ui.focus = Focus::Transcript {
                    sel: sel.saturating_sub(1),
                };
                None
            }
            InputEvent::ArrowDown | InputEvent::Char('j') => {
                let max = ui.conv.turns.len().saturating_sub(1);
                ui.focus = Focus::Transcript {
                    sel: (sel + 1).min(max),
                };
                None
            }
            InputEvent::Enter => {
                toggle_turn(ui, sel);
                None
            }
            InputEvent::Escape => {
                ui.focus = Focus::Input;
                None
            }
            InputEvent::CtrlQ => {
                ui.quit = true;
                None
            }
            InputEvent::CtrlL => {
                ui.repaint = true;
                None
            }
            InputEvent::CtrlC => {
                if ui.active {
                    cancel_flag.store(true, Ordering::SeqCst);
                    ui.status = "cancelling…".into();
                }
                None
            }
            _ => None,
        },
    }
}

/// Handle a mouse click: a left click in the transcript toggles the turn under
/// the cursor (the hit map maps the row to its transcript line → turn).
fn handle_mouse(
    ui: &mut Ui,
    m: &MouseEvent,
    last_hit: &[usize],
    turn_of_line: &[usize],
    transcript: Rect,
) {
    use crossterm::event::{MouseButton, MouseEventKind};
    if m.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    if m.row < transcript.y || m.row >= transcript.y + transcript.height {
        return;
    }
    let rel = (m.row - transcript.y) as usize;
    let Some(&line) = last_hit.get(rel) else {
        return;
    };
    let Some(&turn) = turn_of_line.get(line) else {
        return;
    };
    toggle_turn(ui, turn);
    ui.focus = Focus::Transcript { sel: turn };
}

/// Toggle a turn's thought-bubble expansion (Q5/Q6: collapse on completion,
/// expand on demand).
fn toggle_turn(ui: &mut Ui, sel: usize) {
    if sel >= ui.conv.turns.len() {
        return;
    }
    let key = format!("t{sel}");
    if ui.expanded.contains(&key) {
        ui.expanded.remove(&key);
    } else {
        ui.expanded.insert(key);
    }
}

/// Scroll the transcript by `n` rows (unpinning from the bottom); the render
/// clamps to the available range and re-pins at the bottom.
fn scroll(ui: &mut Ui, n: usize, down: bool) {
    ui.pinned = false;
    if down {
        ui.scroll_top += n;
    } else {
        ui.scroll_top = ui.scroll_top.saturating_sub(n);
    }
}

/// Render one frame: transcript viewport, input/approval line, status bar.
fn draw(
    f: &mut ratatui::Frame,
    ui: &Ui,
    styled: &[StyledRow],
    transcript: Rect,
    input_area: Rect,
    status_area: Rect,
    theme: &Theme,
) {
    // 1. transcript (bottom-pinned viewport over the whole log, R-19).
    f.render_widget(transcript_paragraph(styled, theme), transcript);

    // 2. input line / approval line.
    let line = if let Some(p) = &ui.pending {
        Line::from(vec![
            Span::styled(
                "⚠ approval ",
                resolve_style(theme, Some("error")).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", p.action), resolve_style(theme, None)),
            Span::styled(format_args(&p.args), resolve_style(theme, Some("status"))),
            Span::styled("  [y]es / [n]o", resolve_style(theme, Some("status"))),
        ])
    } else {
        build_input_line(ui, theme)
    };
    f.render_widget(Paragraph::new(Text::from(vec![line])), input_area);

    // 3. status bar: state · model · tokens · hints.
    let (tin, tout) = ui.conv.tokens();
    let state = if ui.active {
        "running"
    } else {
        &ui.status
    };
    let hints = if ui.pending.is_some() {
        "y approve · n deny"
    } else if ui.focus.is_browse() {
        "↑/k · ↓/j · Enter toggle · Esc back"
    } else {
        "Enter send · Esc browse · ↑↓ scroll · Ctrl-C cancel · Ctrl-Q quit"
    };
    let status = Line::from(vec![
        Span::styled(
            state.to_string(),
            resolve_style(theme, Some("progress")).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ·  {}  ·  {}↓ {}↑ tok", ui.model, tout, tin),
            resolve_style(theme, Some("status")),
        ),
        Span::styled(format!("  ·  {hints}"), resolve_style(theme, Some("status"))),
    ]);
    f.render_widget(
        Paragraph::new(Text::from(vec![status])).style(resolve_style(theme, Some("status"))),
        status_area,
    );
}

/// The editable input line with a block cursor at `ui.cursor`.
fn build_input_line(ui: &Ui, theme: &Theme) -> Line<'static> {
    let (before, cur, after) = split_chars(&ui.input, ui.cursor);
    let default = resolve_style(theme, None);
    let mut spans = vec![
        Span::styled(
            "❯ ",
            resolve_style(theme, Some("user")).add_modifier(Modifier::BOLD),
        ),
        Span::styled(before, default),
    ];
    if let Some(c) = cur {
        spans.push(Span::styled(c.to_string(), default.reversed()));
    } else {
        spans.push(Span::styled(" ", default.reversed()));
    }
    if !after.is_empty() {
        spans.push(Span::styled(after, default));
    }
    Line::from(spans)
}

/// Split a string at a character index into (before, cursor char, after).
fn split_chars(s: &str, idx: usize) -> (String, Option<char>, String) {
    let chars: Vec<char> = s.chars().collect();
    if idx >= chars.len() {
        return (s.to_string(), None, String::new());
    }
    let before: String = chars[..idx].iter().collect();
    let cur = chars[idx];
    let after: String = chars[idx + 1..].iter().collect();
    (before, Some(cur), after)
}

/// Byte offset of the character at `idx` (or the end).
fn char_offset(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Render a parked approval's arguments to a compact, single-line string.
fn format_args(args: &str) -> String {
    let s = args.trim();
    if s.is_empty() {
        return "(no args)".into();
    }
    let s = s.replace('\n', " ");
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 60 {
        return s;
    }
    let mut out: String = chars[..60].iter().collect();
    out.push_str("…");
    out
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
    let engine = match build_engine(&opts) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("kanbei: {e}");
            exit(2);
        }
    };
    let code = if std::io::stdin().is_terminal() {
        run_tui(opts, engine)
    } else {
        run_repl(opts, engine);
        0
    };
    exit(code);
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
