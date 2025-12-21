# Holon Terminology (Draft)

This page defines the **public terms** used across Holon docs. It is written to make the project easier to understand before we lock APIs/CLIs.

## Core

### Holon Run
A single, headless execution that:
- reads inputs (spec + context + sandbox workspace)
- invokes an agent runtime
- writes standardized outputs under `/holon/output`

### Holon Runner
The supervisor that starts a Holon Run (today: `holon` CLI and the GitHub Action). It:
- prepares mounts and environment variables
- enforces hard sandbox semantics (e.g. snapshot workspace)
- validates required outputs

### Task Spec (Spec)
The task definition file (e.g. `spec.yaml`). It contains:
- the goal (what to do)
- required outputs (what must be produced)
- optional context hints (focus files, non-secret env)

## Execution Unit

### Holon Agent
The **execution unit** that implements the Holon I/O contract inside the container. It:
- reads `/holon/input/*` and `/holon/workspace`
- drives an underlying **engine** headlessly
- writes outputs to `/holon/output`

### Agent Bundle (Distribution)
How a Holon Agent is shipped/installed.
- Today: a Docker image (current implementation)
- Future: an npm package (install-at-run), or a single binary

The key idea: **the contract is stable**, the distribution format can evolve.

### Engine
The underlying AI tool/runtime controlled by the agent (e.g. Claude Code runtime, Codex CLI). Engines differ in:
- tool availability and behavior
- session model (plan/execution/review capabilities)
- configuration surface (MCP, auth, etc.)

## Controls

### Mode
The run intent with **hard semantics** (e.g. required outputs, workspace RO/RW policy).
- `execute`: produce changes (usually `diff.patch`)
- `plan`: propose changes only
- `review`: produce review feedback only

`mode` is meant to be the primary user-facing selector (see `docs/modes.md`).

### Role
The workflow role aligned with how teams collaborate (prompt-level guidance and output framing).
- `developer` (default)
- `reviewer`

Role influences *how* the agent reasons and reports; `mode` defines *what is allowed/required*.

## Inputs & Outputs

### Sandbox Workspace (Snapshot)
`/holon/workspace` is a sandbox copy of the repo prepared by the Runner. By default, the original checkout is not modified; changes leave the sandbox via outputs (e.g. `diff.patch`).

### Context Pack
Optional files mounted at `/holon/input/context/` (issue text, PR diff, logs, docs excerpts). The caller/workflow decides what to include.

### Outputs (Artifacts)
Standard files under `/holon/output/` such as:
- `manifest.json`: machine-readable status/metadata
- `diff.patch`: patch representing changes
- `summary.md`: human-readable report

## Integrations

### Publisher (External)
Automation that consumes outputs and applies them to external systems (e.g. apply patch + create/update PR, post a comment, create a PR review). We keep publishers outside the agent for clearer permissions and auditability.

## Legacy / Migration Notes

Some internal code/docs may still use older names:
- “adapter” → Holon Agent
- “adapter image” → Agent Bundle (archive in current implementation)
- “runtime/host” → Runner

Implementation plan:
- `docs/refactor-terminology-plan.md`
