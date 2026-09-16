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
cargo run -p kanbei-cli --bin kanbei -- [DIR]
```

- `DIR` (or `$KANBEI_DIR`, default `.`) — the project root and fs sandbox. An
  explicit root keeps the legacy session layout under it; without one, session
  storage follows the XDG state layout (`$XDG_STATE_HOME/kanbei/sessions/<SessionId>/`,
  falling back to `$HOME/.local/state/kanbei`; decision 33). Reopening the same
  session dir resumes the same session.
- Provider and approval wiring come from the merged config layers (built-in
  defaults, then `$XDG_CONFIG_HOME/kanbei/init.lua`, then
  `<DIR>/.kanbei/init.lua`) — `provider.base_url`/`provider.model`/`provider.fake`
  and `approval.auto_approve`/`approval.yolo` (decision 28). The argv flags and
  most `KANBEI_*` env vars were retired: only `$KANBEI_DIR`,
  `$KANBEI_PROVIDER_URL` and `$KANBEI_PROVIDER_KEY` remain, the latter two read
  solely as fallbacks when config supplies no base URL or key (config wins).

On a TTY the CLI runs a full-screen TUI; piped stdin falls back to the plain
REPL.

**TUI.** Launch is always resume: the transcript is a projection service
(`crates/kanbei-transcript`, decision 30) driven by the session over its
committed envelopes and replayed from the canonical log on start. A
turn's working segment renders as a thought bubble — expanded while the turn
runs (live tool steps + streaming text), then collapsed to a summary line
(`state · steps · runs · tokens`, plus the reason on a non-clean end). Reopen
any turn by clicking its summary or selecting it (arrows/`j`/`k`) and pressing
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
Gated tools prompt interactively unless auto-approval is enabled
(`approval.auto_approve` / `approval.yolo` in the config layers). Commands:
`/status`, `/history [N]`, `/export DIR`, `/resume` (after a breaker pause),
`/exit`.

Embedding: `kanbei_driver::Driver::user_turn(text)` returns
`Turn { answer, runs, last_outcome }`; the gates in `crates/kanbei-testkit`
and the `workbench` binary (M7 input-path dogfood) are reference drivers.

Building the guest wasm (`cargo xtask build-guest`) enables live Luau modules
and the built-in UI; a checkout without it FAILS the module tests rather than
skipping them.

## Design documents

- `docs/high-level-architecture.md` — architecture constitution
- `docs/architecture.md` — detailed design ledger
- `docs/design-review-handoff.md` — design review handoff
- `docs/review-reconciliation.md` — review reconciliation record

## License

MIT — see [LICENSE](LICENSE).
