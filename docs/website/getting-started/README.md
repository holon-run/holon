---
title: Getting started
summary: Install Holon from source, inspect the runtime, and run the docs site.
order: 10
---

# Getting started

Holon is currently developed as a Rust runtime. The fastest way to understand
it is to run the binary locally, inspect the repository docs, and keep the
runtime model in view while experimenting.

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
