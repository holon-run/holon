---
title: CLI
summary: How to inspect and use the current Holon command-line surface.
order: 10
---

# CLI

Holon's CLI is the local entrypoint for the Rust runtime. Because the runtime is
still defining its core model, treat the compiled binary as the source of truth
for exact flags and subcommands.

## Inspect help

```bash
cargo run -- --help
```

For a release binary, use the binary directly:

```bash
holon --help
```

## Development checks

```bash
cargo fmt --check
cargo test
```

## Stability expectation

The CLI should make runtime concepts explicit instead of hiding them behind UI
shortcuts. When a command starts work, waits, wakes, supervises a task, or
delivers user-facing output, that lifecycle should be visible in names, logs,
or structured output.
