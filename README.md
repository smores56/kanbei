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
- The REPL takes one user message per line and drives the resulting wakes to
  quiescence; the model's final answer is printed to stdout. Intermediate tool
  round-trips are canonical facts (inspect with `/history`). Gated tools
  prompt interactively unless `--auto-approve` is set.
- Commands: `/status`, `/history [N]`, `/export DIR`, `/resume` (after a
  breaker pause), `/exit`.

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
