# Agent Home Persona Redesign

## Background

Current `holon agent init` persona files are too thin and inconsistent:

- Uses `AGENT.md` while project and ecosystem conventions prefer `AGENTS.md`
- Template contents are one-line stubs and do not provide operational guidance
- Contract prompt references outdated file naming and lacks clear execution/reporting sections

## Goals

1. Unify persona file naming to `AGENTS.md`
2. Keep `AGENTS.md` as canonical and add `CLAUDE.md` as a compatibility redirect
3. Upgrade built-in templates into actionable role playbooks for:
   - `default`
   - `github-solver`
   - `autonomous`
4. Keep runtime behavior aligned with agent-home model:
   - Holon contract explains file responsibilities
   - Persona files are read/written by agent directly from `HOLON_AGENT_HOME`
   - Holon does not inline persona content into the system prompt

## Non-Goals

- Backward compatibility for old `AGENT.md` naming
- New config schema changes in `agent.yaml`
- New runtime modes

## Design

### 1. Persona File Set

`holon agent init` and `EnsureLayout*` generate the following files:

- `AGENTS.md` (canonical persona and operating protocol)
- `CLAUDE.md` (compatibility pointer to `AGENTS.md`)
- `ROLE.md` (current mission and scope)
- `IDENTITY.md` (identity and collaboration defaults)
- `SOUL.md` (decision principles)

`CLAUDE.md` stays minimal and must not become a second source of truth.

### 2. Template Content Structure

Each built-in template should include:

- Mission and scope
- Execution loop (plan -> execute -> verify -> report)
- Quality bar and failure handling
- Role-specific workflow details

#### default

Focuses on one-off execution with deterministic outputs and strict verification.

#### github-solver

Focuses on issue/PR solving workflow: context collection, patching discipline, review feedback handling.

#### autonomous

Focuses on long-lived event-driven operation, continuity, and anti-drift behavior.

### 3. Contract Prompt Structure

Refactor `pkg/prompt/assets/contracts/common.md` into explicit sections:

1. Environment
2. Filesystem & outputs
3. Agent-home protocol
4. Headless execution rules
5. Reporting requirements
6. Context handling

The contract should reference `AGENTS.md` (not `AGENT.md`) and reinforce that persona files are loaded from `HOLON_AGENT_HOME`.

## Implementation Plan

1. Update `pkg/agenthome/agenthome.go`
   - Replace `AGENT.md` with `AGENTS.md`
   - Add `CLAUDE.md` to template output
   - Rewrite template bodies with actionable guidance
2. Update tests in `pkg/agenthome/agenthome_test.go`
   - Assert `AGENTS.md` and `CLAUDE.md`
   - Rename/adjust directory conflict test cases
3. Update CLI/help text and serve docs strings
   - `cmd/holon/agent.go` force help text
   - `cmd/holon/serve.go` agent-home file list
4. Update contract prompt
   - `pkg/prompt/assets/contracts/common.md`
5. Update docs mentioning `AGENT.md`
   - `docs/development.md`
   - `docs/agent-home-unification.md`
   - Any other direct references from repo search

## Acceptance Criteria

1. `holon agent init` generates `AGENTS.md` and `CLAUDE.md` by default
2. No `AGENT.md` references remain in runtime contract or init help text
3. Built-in templates contain operationally useful instructions
4. `go test ./pkg/agenthome ./cmd/holon` passes
5. Repo docs consistently describe `AGENTS.md` as canonical
