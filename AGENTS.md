# Repository Guidelines

`Holon` is an early-stage headless runtime project. Keep the codebase small,
explicit, and easy to reason about.

## Project Structure & Module Organization

- `src/`: Rust runtime implementation and executable entrypoints.
- `tests/`: Rust integration tests and shared test support.
- `builtin_templates/`: runtime-managed agent templates.
- `benchmark/` and `benchmarks/`: benchmark harness, fixtures, suites, and task manifests.
- `docs/`: current runtime contracts, architecture notes, and design records.
- `holonbot/`: Node-based GitHub App/bot retained as a separate service asset.
- `skills/`: repository skills that remain useful outside the old Go runtime.
- `README.md`: public-facing project definition.

Do not introduce large framework scaffolding before the runtime model is clear.

## Product Intent

Holon is meant to be:

- headless
- event-driven
- long-lived
- explicit about trust boundaries
- explicit about user-facing versus internal output

When design choices conflict, prefer runtime clarity over convenience.

## Development Priorities

Prioritize work in this order:

1. Runtime model and message envelope.
2. Queue, wake, sleep, and task lifecycle.
3. Event ingress and trust classification.
4. Structured user-facing output.
5. Integrations and adapters.

Do not start with UI work unless the task explicitly requires it.

## Coding Style & Naming Conventions

- Prefer simple, direct modules over indirection-heavy abstractions.
- Make state transitions explicit in code and types.
- Name modules after runtime responsibilities, not implementation accidents.
- Keep comments short and only where state or lifecycle behavior is non-obvious.
- Avoid hidden background behavior. If something wakes, retries, sleeps, or
  enqueues work, make that visible in names and logs.

## Architecture Guardrails

- Treat `origin`, `trust`, and `priority` as first-class runtime concepts.
- Keep `brief` or user-facing delivery separate from internal execution traces.
- Do not mix operator input and external channel input without preserving
  provenance.
- Prefer append-only event/state logs when possible over opaque mutable state.
- Avoid coupling the core runtime to any single model vendor or UI surface.

## Documentation Expectations

When changing the project definition, update the relevant entry docs such as
`README.md`, `docs/project-goals.md`, `docs/runtime-spec.md`, `docs/roadmap.md`,
or `docs/coding-roadmap.md`.

If a change affects runtime contracts or architecture, add or update the
relevant document under `docs/rfcs/` before or alongside implementation. Keep
historical or one-off notes out of the top-level `docs/` surface when
possible.

If implementation work has multiple reasonable choices, and the final choice
matters for future maintenance but the reasoning cannot be expressed clearly in
code, add one short focused note under `docs/implementation-decisions/`.
Prefer one decision per file and keep the note limited to the choice, the
reason, and the preserved boundary or tradeoff.

Before architecture or roadmap work, review the relevant current RFCs under
`docs/rfcs/` and the current GitHub issues. Do not treat archived docs as the
current source of truth.

## Commit & Pull Request Guidelines

- Use clear conventional commit prefixes: `feat:`, `fix:`, `docs:`, `refactor:`,
  `test:`, `chore:`.
- Keep early commits narrow. This repository is still defining its core model.
- In PR descriptions, explain which runtime concept changed and why.

## Migration Notes

- The main runtime is now the Rust `holon` binary produced by Cargo.
- Do not reintroduce the old Go CLI/runtime path while adapting workflows.
- GitHub workflow and release automation should be rebuilt around the Rust binary.
- The old TypeScript Claude agent bundle has been removed; runtime integration
  should go through the Rust provider/tooling model instead of a separate
  Claude SDK bundle.
- `holonbot` remains a separate service asset; do not couple it back into the
  runtime core without a specific design decision.

## What To Avoid

- Premature plugin systems
- UI-first architecture
- Hidden global state
- Implicit trust elevation
- Tight coupling between scheduling and transport-specific adapters
