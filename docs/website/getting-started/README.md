---
title: Getting started
summary: Install Holon from source, inspect the runtime, and run the docs site.
order: 10
---

# Getting started

Holon is currently developed as a Rust runtime. The fastest way to understand
it is to run the binary locally, inspect the repository docs, and keep the
runtime model in view while experimenting.

## New to Holon?

If this is your first time using Holon, follow our step-by-step tutorial:

- **[Create your first agent](first-agent.md)** - Build, start, connect with TUI, create an agent, and configure models in ~15 minutes

This hands-on guide covers:

- Building Holon from source
- Starting the runtime (CLI mode and daemon mode)
- Using the Terminal UI (TUI)
- Creating agents with templates
- Configuring models and providers

## Experienced developers?

If you're already familiar with Holon or want to jump straight into specifics:

- **[Runtime model](/concepts/runtime-model.md)** - Understand Holon's core concepts
- **[Trust boundaries](/concepts/trust-boundaries.md)** - Learn about security and isolation
- **[Local runtime guide](/guides/local-runtime.md)** - Conservative workflow for local development
- **[Work items guide](/guides/work-items.md)** - Track durable objectives across turns
- **[Quick examples](/guides/quick-examples.md)** - Common tasks and workflows
- **[Integration guide](/guides/integration.md)** - Integrate Holon into your systems
- **[Troubleshooting guide](/guides/troubleshooting.md)** - Diagnose common setup and runtime issues
- **[CLI reference](/reference/cli.md)** - Command-line interface details
- **[HTTP control plane](/reference/http-control-plane.md)** - REST API for automation

## Requirements

- Rust toolchain with Cargo.
- Node.js only when you are working on this mdorigin documentation site.
- A model/provider configuration appropriate for the local runtime commands you
  intend to exercise.

## Build from source

```bash
git clone https://github.com/holon-run/holon.git
cd holon
cargo build
```

Run the binary help to see the currently compiled command surface:

```bash
cargo run -- --help
```

## Learn the runtime vocabulary

Before wiring Holon into an integration, read:

- [Runtime model](/concepts/runtime-model.md)
- [Trust boundaries](/concepts/trust-boundaries.md)
- [Local runtime guide](/guides/local-runtime.md)

The key idea is that Holon is not just a request/response wrapper around a
model. It tracks work, tasks, queues, wake conditions, and delivery surfaces as
runtime state.

## Run the documentation site

The `docs/website/` directory is a mdorigin content root:

```bash
cd docs/website
mdorigin dev --root .
```

Useful build checks:

```bash
mdorigin build index --root .
mdorigin build search --root . --out dist/search
mdorigin build cloudflare --root . --search dist/search
```

The generated `dist/` directory is ignored because it is a build artifact.

## Repository orientation

- `src/` contains the Rust runtime implementation and executable entrypoints.
- `tests/` contains Rust integration tests and shared support.
- `docs/` contains runtime contracts, design records, and current architecture
  notes.
- `builtin_templates/` contains runtime-managed agent templates.
- `docs/website/` contains this mdorigin documentation site.

<!-- INDEX:START -->

<!-- INDEX:END -->
