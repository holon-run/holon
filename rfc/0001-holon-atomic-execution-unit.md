# RFC-0001: Holon Protocol (v0.1)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Superseded** |
| **Author** | Holon Contributors |
| **Created** | 2025-12-16 |
| **Updated** | 2026-02-17 |
| **Target Version** | v0.1 |
| **Superseded By** | RFC-0002, RFC-0003 |

## 1. Summary

This RFC defines the **Holon Protocol** for v0.1: a stable, tool-agnostic way to describe a task (**Spec**) and collect results (**Artifacts**) from a headless agent execution inside a sandbox.

This document is intended to be **normative** (what runners/agents MUST do). Design notes and reference implementations live under `docs/` (non-normative), e.g. `docs/holon-architecture.md`.

## Implementation Reality (2026-02-17)

- This RFC is kept as historical context for the v0.1 model.
- Current contract direction is defined by RFC-0002 and RFC-0003.
- Runtime/home/workspace semantics have evolved beyond the fixed v0.1 assumptions in this document.

## 2. Core concepts (terms)

### 2.1 Holon (The Unit)
A Holon is a single execution attempt of a defined engineering task. It has a binary outcome state: `Success`, `Failure`, or `NeedsHumanReview`.

### 2.2 Holon Spec (The Input)
A declarative document (YAML/JSON) defining the **Goal** and expected **Artifacts**. Context can include environment variables and a list of relevant files.

### 2.3 Agent (The Execution Unit)
The container entrypoint that reads Holon inputs and drives an underlying engine/runtime headlessly, producing standard artifacts.

### 2.4 Runner (The Supervisor)
The supervisor (typically the `holon` CLI) that prepares inputs, runs the agent container, validates artifacts, and hands results to callers/workflows.

## 3. Spec schema (v1)

The `spec.yaml` defines the task. Fields are intended to be forward-compatible; unknown fields SHOULD be ignored by agents.

```yaml
version: "v1"
kind: Holon

metadata:
  name: "task-name"        # Human-readable slug
  id: "uuid-optional"      # Tracking ID

# INPUT: The context provided to the Agent
context:
  workspace: "${HOLON_WORKSPACE_DIR}"  # Optional. Typical container path: /root/workspace.
  files:                   # Priority files to focus on
    - "src/main.go"
    - "README.md"
  env:                     # Non-secret environment variables
    TEST_MODE: "true"

# GOAL: What needs to be done
goal:
  description: "Fix the nil pointer exception in Handler"
  issue_id: "GH-123"       # Optional reference

# OUTPUT: Required deliverables
output:
  artifacts:
    - path: "diff.patch"
      required: true
    - path: "summary.md"
      required: true
    - path: "tests.log"
      required: false

# CONSTRAINTS: Execution boundaries
constraints:
  timeout: "10m"
  max_steps: 50            # Agent step limit (if supported)
```

## 4. Container filesystem layout (Normative)

Holon exposes standardized runtime directories via environment variables. Typical container defaults are under `/root`.

### 4.1 Workspace
- `HOLON_WORKSPACE_DIR` (typically `/root/workspace`): workspace snapshot root (runner sets container `WorkingDir` to this path)

### 4.2 Inputs
Runners MUST mount:
- `${HOLON_INPUT_DIR}/spec.yaml` (read-only): the Holon spec.
- `${HOLON_INPUT_DIR}/context/` (read-only, optional): injected context files.

Runners MAY mount:
- `${HOLON_INPUT_DIR}/prompts/system.md` (read-only): compiled system prompt.
- `${HOLON_INPUT_DIR}/prompts/user.md` (read-only): compiled user prompt.

Secrets MUST be injected via environment variables (not in spec).

### 4.3 Outputs
Agents MUST write all outputs under:
- `HOLON_OUTPUT_DIR` (typically `/root/output`, read-write): artifacts such as `manifest.json`, `diff.patch`, `summary.md`, and optional `evidence/`.

The Runner SHOULD treat `HOLON_OUTPUT_DIR` as the integration boundary and SHOULD ensure it starts empty for each run to avoid cross-run contamination.

## 5. Execution lifecycle (Normative)

A Holon execution is **single-shot**:
- the agent process starts, performs the task, writes artifacts, and terminates,
- the agent MUST NOT require inbound ports or run as a daemon.

Exit codes and artifact requirements are defined in `rfc/0002-agent-scheme.md`.

## 6. Security & network (v0.1)

v0.1 assumes network access is available for calling LLM APIs. Future versions may add stricter egress controls.

## 7. Non-normative references

Design notes and examples (non-normative):
- Architecture/design: `docs/holon-architecture.md`
- Agent pattern (non-normative): `docs/agent-encapsulation.md`
- `mode` design (solve/plan/review): `docs/modes.md`
