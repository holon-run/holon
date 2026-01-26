# RFC-0002: Agent Contract (v0.2)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Active** |
| **Author** | Holon Contributors |
| **Created** | 2025-12-18 |
| **Updated** | 2026-01-26 |
| **Parent** | RFC-0001 |

## 1. Summary

This RFC defines the **Holon Agent Contract** for v0.2: a stable, tool-agnostic interface that allows different engines to be plugged into Holon while supporting skill-owned artifacts and a minimal runtime boundary.

This document is intended to be **normative** (what agents/runners MUST do). Reference implementations and composition details live under `docs/` (non-normative).

### v0.2 Changes (2026-01-26)

- **Skill-first artifacts**: Artifacts are owned by skills; Holon only standardizes the minimal runtime boundary (filesystem layout + execution record)
- **Relaxed filename requirements**: `manifest.json` is the only required artifact; `diff.patch` and `summary.md` are recommended for code-review skills, not universally required
- **Backward compatibility**: Existing solve/pr-fix flows continue to work with maintained behavior

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

#### 3.2.1 Required artifacts

Agents MUST produce at minimum:
- `manifest.json` (required): machine-readable metadata about the run (status/outcome/duration/artifacts + tool/runtime metadata).

#### 3.2.2 Skill-defined artifacts

All other artifacts are **skill-defined**. Skills MAY produce any artifacts with any names and formats under `/holon/output/`.

For **code workflow skills** (e.g., issue-to-PR, PR-fix), the following artifacts are **RECOMMENDED** but not required:
- `diff.patch` (recommended for code workflows): a patch representing workspace changes, compatible with `git apply` workflows.
- `summary.md` (recommended for code workflows): a human-readable report of the execution.
- `evidence/` (optional): logs and verification output.

Other skill types (e.g., bots, assistants, automation tools) define their own artifact conventions.

#### 3.2.3 Artifact validation

Runners MUST validate that `manifest.json` exists. Runners MAY validate spec-defined artifacts (see Section 4.1).

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

### 3.5 Patch requirements (recommended for code skills)

When a skill produces `diff.patch` (e.g., for code-review workflows), the patch SHOULD be compatible with `git apply` workflows.

For binary-file compatibility, skills SHOULD generate patches using `git diff --binary --full-index` (or equivalent).

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
- validate that `manifest.json` exists (and treat a missing manifest as a run failure).

### 4.1 Optional artifact validation

Runners MAY validate artifacts listed in `spec.output.artifacts[]` if the spec defines them. Missing artifacts marked as `required: true` SHOULD be treated as a run failure.

### 4.2 Backward compatibility

For backward compatibility with v0.1 workflows:
- Runners MAY continue to support spec-defined artifact lists (e.g., `diff.patch`, `summary.md`) as required for code-review modes.
- Existing `solve` and `pr-fix` flows MUST maintain their current behavior unless explicitly migrated to skill-first artifacts.

Applying changes back to the original repo (e.g., `git apply` + commit + PR) is an explicit caller/workflow step and MUST NOT be implicit side effects of the agent.

## 5. Migration and Compatibility

### 5.1 v0.1 to v0.2 migration

The v0.2 contract is **backward compatible** with v0.1:
- v0.1 agents that produce `manifest.json`, `diff.patch`, and `summary.md` remain valid.
- v0.2 agents MAY produce skill-defined artifacts beyond these standard names.

Runners supporting v0.2:
- MUST require `manifest.json`
- MAY validate spec-defined artifacts if present
- SHOULD NOT require specific artifact names beyond `manifest.json` for skill-first flows

### 5.2 Skill authoring guidance

For guidance on defining skill-specific artifacts, see:
- `docs/skills.md` - Skills overview and usage
- `docs/manifest-format.md` - Manifest format and artifact declaration

## 6. Non-normative references

Implementation details and examples:
- Agent encapsulation scheme: `docs/agent-encapsulation.md`
- Claude agent reference notes: `docs/agent-claude.md`
- High-level architecture and composition notes: `docs/holon-architecture.md`
- `mode` design (solve/plan/review): `docs/modes.md`
- Skill-first architecture: `rfc/0003-skill-artifact-architecture.md`
