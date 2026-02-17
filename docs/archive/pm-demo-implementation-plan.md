# PM Role Demo Implementation Plan

This document defines the concrete implementation plan for a runnable PM-role demo.

## Goal

Run two persistent agents with distinct identities:

1. PM agent plans and updates roadmap.
2. PM agent creates/assigns issues and coordinates PR flow.
3. Dev agent listens to assigned tasks and implements code changes.
4. PM agent reviews/merges and iterates until goal completion.

Scope for this document is demo-first, not hardening-first.

## Product Shape (Demo)

1. Two `holon serve` processes: PM and Dev.
2. One controller instance per process.
3. Distinct GitHub identities for PM and Dev agents.
4. Event-driven loop via GitHub webhook + timer tick.
5. Role-driven behavior (`pm` / `dev`) with a shared controller runtime skill.

No multi-role scheduler in this phase.

## Key Design Choice

PM remains agent-first and free-form in reasoning (not fixed intent-only).

Implementation is not delegated through nested Holon process spawning.

Instead:

1. PM delegates by assigning GitHub issues to Dev identity.
2. Dev agent reacts to assignment events and executes issue implementation.
3. Collaboration happens through normal GitHub issue/PR lifecycle.

This keeps collaboration explicit and auditable in GitHub.

## Runtime Data Flow

1. Ingress event arrives (`github.*` or `timer.tick`).
2. `serve` forwards normalized event to controller channel.
3. PM role decides and performs GitHub management actions (plan/create/assign/review/merge).
4. Dev role processes issues assigned to Dev identity and opens/updates PRs.
5. PM consumes resulting issue/PR/check/review events and continues planning.

## Minimal Artifacts

Under `--state-dir`:

- `events.ndjson`
- `decisions.ndjson`
- `actions.ndjson`
- `controller-state/event-channel.ndjson`
- `controller-state/goal-state.json` (new)

## Planned Code Changes

### 1) Dual controller deployment

Files:

- `cmd/holon/serve.go`
- `skills/github-controller/` (shared runtime loop skill)
- `pkg/prompt/assets/roles/pm.md` (new role)
- `pkg/prompt/assets/roles/dev.md` (new role)

Add/Adjust:

1. Run PM and Dev controllers as separate serve processes.
2. Ensure each process has separate state/workspace directories.
3. Ensure each process runs with its own GitHub token/identity.
4. Keep one shared controller skill; select behavior by role.

### 2) PM role definition

PM role responsibilities:

1. Maintain roadmap and priorities.
2. Create issues from plan and assign implementation tasks to Dev identity.
3. Review Dev PRs and merge when ready.
4. Update `goal-state.json` after each major planning/review cycle.

### 3) Dev role definition

Dev role responsibilities:

1. React only to issues assigned to Dev identity (or labeled for Dev lane).
2. Execute implementation using existing solve/fix workflows.
3. Open/update PR and report progress back via issue/PR comments.
4. Ignore unrelated events.

### 4) Serve prompt wiring

File: `cmd/holon/serve.go` (`writeControllerSpecAndPrompts`)

Support role-specific prompt framing:

1. PM prompt: planning/assignment/review/merge loop.
2. Dev prompt: assignment-driven implementation loop.
3. Keep role behavior boundaries explicit in prompt contracts.
4. Controller skill remains generic event-loop runtime.

### 5) Timer source

File: `cmd/holon/serve.go` or `pkg/serve/service.go`

Add optional periodic tick event emitter:

1. Flag example: `--tick-interval 5m`
2. Emits `timer.tick` envelopes into the same processing pipeline.

### 6) Goal acquisition flow

Goal is not passed as a dedicated CLI parameter.

PM discovers and evolves goal from:

1. Repository source of truth documents (README/RFC/docs/issues).
2. Existing roadmap/meta issues.
3. Operator messages from conversation events/comments.

Persist normalized working goal in `controller-state/goal-state.json`.

### 7) Documentation and demo runner

Add:

1. `docs/archive/pm-dev-demo.md` (how to run end-to-end).
2. Optional script: `scripts/demo/pm-loop.sh`.

## CLI/Runtime Inputs (Demo)

Suggested new flags:

1. `--tick-interval <duration>`: periodic planning tick.
2. `--agent-id <id>`: select controller identity and load role from `<agent_home>/ROLE.md`.
3. `--webhook-port <port>`: ingress/rpc port override.

## Demo Runbook

1. Create a new test repo and provide goal context in repo docs and/or a meta issue.
2. Start PM serve in webhook mode with PM identity/token.
3. Start Dev serve in webhook mode with Dev identity/token.
4. Forward GitHub events to both local endpoints.
5. Let PM create/assign issues and Dev implement via PRs.
6. Observe iterative PR flow and merges until goal completion.

## Success Criteria (Demo)

1. PM can autonomously create and sequence issue work.
2. PM assigns at least one issue to Dev identity automatically.
3. Dev opens/updates PR for assigned work.
4. PM reviews/merges and continues planning without manual step-by-step steering.

## Out of Scope (Now)

1. Multi-instance/multi-role scheduler.
2. Strict policy/approval hardening.
3. Full generalized planner DSL.
4. Long-term hosted control plane reliability work.
