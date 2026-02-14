# RFC-0003: Skill-Artifact Architecture

| Metadata | Value |
| :--- | :--- |
| **Status** | **Active** |
| **Author** | Holon Contributors |
| **Created** | 2026-01-06 |
| **Updated** | 2026-01-26 |
| **Parent** | RFC-0001, RFC-0002 |
| **Issue** | [#433](https://github.com/holon-run/holon/issues/433) |

## 1. Summary

This RFC proposes a **skill-first** architecture where the agent (via skills) owns end-to-end task semantics — including **context preparation** and **publishing side effects** — while Holon focuses on providing a **standard, isolated runtime** (one-shot or long-lived).

Key changes:

1. **Move context prepare into skills**: the agent can fetch/assemble required context in-container (e.g., via `gh`).
2. **Move publishing into skills**: the agent can publish via skill-provided scripts/tools (PRs/comments/messages/etc.).
3. **Artifacts are skill-defined**: Holon no longer standardizes output filenames beyond a minimal filesystem contract; “code workflow” artifacts (e.g., patches/summaries) become **recommended**, not required.
4. **Unify solve/pr-fix as a single skill** (e.g., `github-solve`) with context-aware behavior.

## 2. Motivation

### 2.1 Current Pain Points

- **Slow iteration**: today, “collect context” and “publish results” live in Holon (runner/workflows). Small behavior tweaks require changing Holon, workflows, and docs together.
- **Mode-centric coupling**: `solve` vs `pr-fix` is largely a packaging choice; behavior is driven by context (Issue vs PR) and desired side effects.
- **Rigid outputs**: standardizing artifacts at the runner level works for code review workflows, but blocks non-code skills (bots, assistants, automation) and makes new scenarios expensive to add.

### 2.2 Design Goals

- **Fast extension**: add new behaviors by shipping/updating skills (instructions + scripts), not by modifying Holon core logic.
- **Runtime neutrality**: Holon remains an engine-agnostic container runtime (Claude Code today; others later).
- **Skill-defined artifacts**: outputs are owned by the skill; Holon provides the isolated filesystem contract and execution record.
- **Supports one-shot and long-lived**: the same runtime contract works for batch “run once” and session-based “serve”.

## 3. Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                           HOLON RUNTIME                          │
│  - provides isolated container execution                          │
│  - mounts HOLON_INPUT_DIR, HOLON_WORKSPACE_DIR, HOLON_OUTPUT_DIR   │
│  - stages skills (builtin/user/remote) for agent-native discovery  │
└───────────────────────────────┬──────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                            AGENT + SKILLS                        │
│  - discovers skills natively (e.g., Claude Code skills)            │
│  - prepares context in-container (optional)                        │
│  - executes task logic and writes skill-defined artifacts          │
│  - publishes side effects via skill scripts/tools (optional)       │
└──────────────────────────────────────────────────────────────────┘
```

## 4. Contract: input/workspace/output (minimal and stable)

This RFC does not redefine the agent/runner contract from RFC-0002; it clarifies the intended direction for a **skill-first** world.

### 4.1 `HOLON_INPUT_DIR` (request envelope; read-only)

- `HOLON_INPUT_DIR` contains the immutable “what to do” payload:
  - trigger payloads (issue/PR ref, webhook event, tick),
  - user message + attachments references (serve/session),
  - high-level constraints (timeouts, budgets, enabled skills).
- When context preparation is owned by skills, `HOLON_INPUT_DIR` is **not** expected to contain a fully materialized context snapshot.
- Skills MUST NOT rely on mutating `HOLON_INPUT_DIR` to pass data between steps; derived context SHOULD be written under `HOLON_OUTPUT_DIR` (or a skill-defined state directory).

### 4.2 `HOLON_WORKSPACE_DIR` (working directory; usually read-write)

- `HOLON_WORKSPACE_DIR` is the working directory for code and data.
- It can be a snapshot, a clone, or an application workspace depending on the runner/app.
- Long-lived service scenarios SHOULD isolate per-turn mutations (e.g., worktrees) to avoid cross-turn contamination.

### 4.3 `HOLON_OUTPUT_DIR` (skill-defined outputs; read-write)

- `HOLON_OUTPUT_DIR` is the only required write sink.
- Skills define their own artifact names, formats, and conventions.
- “Code workflow” skills MAY emit patches/summaries for review, but Holon should not require any specific filenames for non-code skills.

## 5. Skill design (current implementation + direction)

### 5.1 Skill naming and references

Holon supports skills as directories with `SKILL.md` and optional supporting files (scripts/templates/etc.).

Skills are discovered by the underlying agent runtime (e.g., Claude Code). In practice, this means skill directories live under a single `skills` root (e.g., `.claude/skills/<skill-name>/SKILL.md`).

As a result, skill references should be **single-directory names** (no nested `platform/scenario` paths) to align with current skill discovery conventions.

Recommended naming scheme:
- `<platform>-<scenario>` (flat), e.g. `github-solve`

### 5.2 Skill staging (agent-native discovery; not prompt injection)

Holon stages resolved skills into the location discovered by the agent runtime (e.g., Claude Code).

Important:
- Holon does **not** inject skill content into compiled prompts in skill mode.
- Skills are discovered natively by the underlying engine (e.g., Claude Code skills).

### 5.3 “Unified solve” skill

The current builtin skill `github-solve` is intended to unify Issue→PR and PR-fix behavior by detecting context.

In the skill-first direction of this RFC, “context detection” can be driven by:
- request envelope fields (e.g., “issue ref” vs “pr ref”),
- or by context prepared in-container by the skill itself.

## 6. Artifact and publish model (skill-owned)

### 6.1 Artifacts are owned by the skill

This RFC deliberately avoids prescribing:
- specific artifact filenames,
- a universal artifact taxonomy,
- a universal “publisher” component in Holon.

Instead:
- each skill defines the artifacts it produces and consumes,
- each skill may include scripts/tools to enact side effects (publishing).

### 6.2 Recommended pattern: “plan as JSON, execute via script” (optional)

Many side effects (PR create/update, comments, messages) are easier to execute reliably if the agent:

1. Writes a structured JSON “intent” file (skill-defined name and schema)
2. Invokes a deterministic skill-provided script/tool to apply that intent

This pattern is:
- recommended for reliability and retryability,
- but not required by Holon.

## 7. Migration path (from runner-owned to skill-owned)

Holon already supports “skill mode” via `--skill/--skills`, but today:
- context preparation and publish are still performed by Holon commands/workflows.

The migration direction is:

1. **Skill-first GitHub flow**: move GitHub context collection and publishing into a builtin GitHub skill (or skill bundle).
2. **Thin Holon core**: Holon focuses on runtime execution and filesystem contract; app semantics live in skills.
3. **Compatibility layer**: keep `holon solve` as a convenience wrapper initially, but reduce its internal coupling over time.

## 8. Minimal Execution Record

### 8.1 Runtime-owned execution record

While artifacts are skill-owned, Holon provides a minimal **always-on execution record** that is stable across all skills and execution modes:

**Required:** `manifest.json` at `${HOLON_OUTPUT_DIR}/manifest.json` (typically `/root/output/manifest.json`)

The manifest contains:
- `status`: Execution status (`"completed"`, `"failed"`, `"running"`)
- `outcome`: Final outcome (`"success"`, `"failure"`, `"needs_human"`)
- `duration`: Execution duration (e.g., `"12.5s"`)
- `artifacts`: List of all output artifact paths generated by the skill
- `metadata`: Optional execution metadata (agent, version, mode, engine info)
- `error`: Error message (only present when outcome is `"failure"`)

This manifest is:
- **Runtime-owned**: Generated by the Holon runtime infrastructure
- **Skill-extensible**: Skills declare their artifacts in the `artifacts` array
- **Stable across skills**: Same format regardless of which skill executed
- **Machine-readable**: Enables automation and orchestration workflows

### 8.2 Optional: Event stream (future)

For debugging and observability, a future enhancement MAY add an event stream artifact:
- `events.ndjson`: Newline-delimited JSON event log for detailed execution tracing

This would be optional and primarily useful for:
- Long-running service mode sessions
- Debugging agent behavior
- Audit trails

### 8.3 Skill artifacts are separate

Skills define their own artifacts beyond `manifest.json`:
- Code skills: `diff.patch`, `summary.md`, `evidence/`
- GitHub skills: `pr-fix.json`, `publish-intent.json`
- Other skills: Any skill-defined outputs

The manifest's `artifacts` array lists ALL outputs, providing a unified inventory.

## 9. Open Questions

### 9.1 Resolved

- ~~What is the minimal always-on execution record?~~ **RESOLVED**: `manifest.json` is the required minimal execution record. Optional `events.ndjson` may be added later for observability.

### 9.2 Pending

1. How should Holon represent a "request envelope" for common triggers (issue/pr refs, webhooks, ticks) without reintroducing modes?
2. Should Holon support aliases for historical skill refs (if any), or keep a single canonical flat name per skill?

## 10. References

- Epic issue: [#433](https://github.com/holon-run/holon/issues/433)
- Parent RFCs: RFC-0001 (Holon atomic execution unit), RFC-0002 (agent contract)
- Related docs:
  - `docs/manifest-format.md` - Manifest schema and format details
  - `docs/skills.md` - Skills documentation and authoring guidance
