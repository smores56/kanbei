# Kanbei

Kanbei is a self-hosted, single-user, always-on agentic assistant built around an
inspectable canonical world state, rebuildable semantic memory, durable execution
loops, and scoped self-modification.

This repository is under active implementation from the approved design in
`docs/design/`.

## Requirements

- Deno 2.9.2
- Git

## Commands

```sh
deno task hooks:install   # enable the committed pre-commit gate
deno task fmt             # format
deno task lint            # deno lint + oxlint
deno task typecheck       # strict TypeScript check
deno task test            # test suite
deno task verify          # all merge-blocking checks
deno task verify:fix      # format and auto-fix, then verify
```

The pre-commit hook runs `deno task verify`. CI runs the same command.

## Engineering contract

See `AGENTS.md` for the Effect-first coding rules, architecture boundaries, and
definition of done.
