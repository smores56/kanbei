# kanbei

A local-first agent harness for expert developers: a small, strongly typed Rust
enforcement kernel with perpetual cognition, live capability-scoped Luau
extensions, and durable, inspectable audit history.

Named after Kuroda Kanbei — a mind in the dungeon that knows only what it is
told, and thinks without stopping.

## Usage

The product surface is the `kanbei` CLI (in `crates/kanbei-cli`), which drives
the cognition driver (`crates/kanbei-driver`) over a durable session
(`crates/kanbei-session`):

```
cargo run -p kanbei-cli --bin kanbei -- [DIR] [--model M] [--fake] [--auto-approve]
```

- `DIR` (or `$KANBEI_DIR`, default `.`) — the session dir: canonical log,
  content-addressed objects, snapshots, and memory roots. Reopening the same
  dir resumes the same session.
- Provider: `$KANBEI_PROVIDER_URL` / `$KANBEI_PROVIDER_KEY` /
  `$KANBEI_PROVIDER_MODEL` (an OpenAI-compatible chat-completions endpoint;
  `--model` overrides the env), or `--fake` for a scripted smoke run.

On a TTY the CLI runs a full-screen TUI; piped stdin falls back to the plain
REPL.

**TUI.** Launch is always resume: the transcript is a live projection of the
session's committed envelopes, rebuilt on start from the canonical log. A
turn's working segment renders as a thought bubble — expanded while the turn
runs (live tool steps), then collapsed to a summary line (`state · steps ·
runs · tokens · elapsed`, plus the reason on a non-clean end). Reopen any
turn by clicking its summary or selecting it (arrows/`j`/`k`) and pressing
`Enter`. The final answer renders below the bubble. The status bar shows
`state · model · egress tokens · key hints`; scrollback covers the whole log
(bottom-pinned, `↑`/`↓`/PageUp/Down to scroll).

- Input: `Enter` sends; `Esc` switches to transcript browse and back.
- Approvals render inline in the transcript (`y` approve / `n` deny); the
  status bar shows `awaiting approval` while the run is parked.
- `Ctrl-C` cancels the active run; `Ctrl-Q` quits (cancelling first if a run
  is active); `Ctrl-L` repaints.

**REPL (piped stdin).** One user message per line; the resulting wakes are
driven to quiescence and the model's final answer is printed to stdout.
Intermediate tool round-trips are canonical facts (inspect with `/history`).
Gated tools prompt interactively unless `--auto-approve` is set. Commands:
`/status`, `/history [N]`, `/export DIR`, `/resume` (after a breaker pause),
`/exit`.

Embedding: `kanbei_driver::Driver::user_turn(text)` returns
`Turn { answer, runs, last_outcome }`; the gates in `crates/kanbei-testkit`
and the `workbench` binary (M7 input-path dogfood) are reference drivers.

Building the guest wasm (`cargo build -p kanbei-guest --target
wasm32-wasip1 --release`) enables live Luau modules and the built-in UI;
without it, module-bound features skip.

## Design documents

- `docs/high-level-architecture.md` — architecture constitution
- `docs/architecture.md` — detailed design ledger
- `docs/design-review-handoff.md` — design review handoff
- `docs/review-reconciliation.md` — review reconciliation record

## License

MIT — see [LICENSE](LICENSE).
