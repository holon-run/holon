# RFC-0002: Agent Contract (v0.1)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2025-12-18 |
| **Parent** | RFC-0001 |

## 1. Summary

This RFC defines the **Holon Agent Contract** for v0.1: a stable, tool-agnostic interface that allows different engines to be plugged into Holon while producing consistent, automatable outputs.

This document is intended to be **normative** (what agents/runners MUST do). Reference implementations and composition details live under `docs/` (non-normative).

## 2. Scope & Terms

- **Runner**: the Holon supervisor (typically the `holon` CLI) that prepares inputs, runs the agent container, and consumes outputs.
- **Agent**: the container entrypoint that reads Holon inputs and drives an underlying engine/runtime headlessly.
- **Engine**: the AI coding runtime controlled by the agent (Claude Code, Codex, etc.).

This RFC builds on `rfc/0001-holon-atomic-execution-unit.md`.

## 3. Agent Contract (Normative)

Every agent MUST implement the following minimum contract.

### 3.1 Inputs

Agents MUST treat these paths as **read-only**:
- `/holon/input/spec.yaml`: the Holon spec.
- `/holon/input/context/` (optional): caller-provided context files (issue text, PR diff, logs, etc.).
- `/holon/input/prompts/system.md` (optional): compiled system prompt (runner-provided).
- `/holon/input/prompts/user.md` (optional): compiled user prompt (runner-provided).

Agents MUST treat the workspace as the codebase root:
- `/holon/workspace`: a workspace **snapshot** prepared by the Runner.

Secrets are provided via environment variables (e.g., `ANTHROPIC_API_KEY`) and MUST NOT be embedded in the spec.

### 3.2 Outputs

Agents MUST write all produced artifacts under `/holon/output/` (read-write).

Agents MAY read files they created under `/holon/output/` during the same run (e.g., incremental notes), but MUST NOT write outputs outside `/holon/output/`.

At minimum, agents MUST support these artifact names:
- `manifest.json` (required): machine-readable metadata about the run (status/outcome/duration/artifacts + tool/runtime metadata).
- `diff.patch` (required when requested by spec): a patch representing workspace changes.
- `summary.md` (required when requested by spec): a human-readable report.
- `evidence/` (optional): logs and verification output.

### 3.3 Exit codes

Agents MUST use the following exit codes:
- `0`: success
- `1`: failure
- `2`: needs human review (optional; if unsupported, report via `manifest.json` and exit `1`)

### 3.4 Headless requirement

Agents MUST run **headlessly**:
- MUST NOT require a TTY.
- MUST NOT block on interactive onboarding, permission prompts, or update prompts.
- MUST fail fast when required credentials/config are missing, and record a clear error in `manifest.json`.

### 3.5 Patch requirements

When `diff.patch` is required by the spec, the agent MUST produce a patch that is compatible with `git apply` workflows.

For binary-file compatibility, agents SHOULD generate patches using `git diff --binary --full-index` (or equivalent).

### 3.6 Probe mode (optional)

Agents MAY implement a probe mode to validate basic runtime readiness without invoking the underlying engine.

If supported, invoking the agent with `--probe` MUST:
- verify `/holon/input/spec.yaml` exists and `/holon/output/` is writable,
- exit with code `0` on success,
- write a minimal `manifest.json` indicating probe success.

Runners MAY use `--probe` to validate bundles/images in CI or preflight checks.

## 4. Runner Responsibilities (Normative)

To preserve atomicity and enable deterministic automation, the Runner MUST:
- mount a **workspace snapshot** at `/holon/workspace` (not the original workspace, by default),
- ensure `/holon/output/` starts empty (fresh dir or cleared) to avoid cross-run contamination,
- validate required artifacts listed in `spec.output.artifacts[]` (and treat missing required artifacts as a run failure).

Applying changes back to the original repo (e.g., `git apply` + commit + PR) is an explicit caller/workflow step and MUST NOT be implicit side effects of the agent.

## 5. Non-normative references

Implementation details and examples:
- Agent encapsulation scheme: `docs/adapter-encapsulation.md`
- Claude agent reference notes: `docs/adapter-claude.md`
- High-level architecture and composition notes: `docs/holon-architecture.md`
- `mode` design (execute/plan/review): `docs/modes.md`
