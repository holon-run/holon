# RFC-0001: Holon Protocol (v0.1)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2025-12-16 |
| **Target Version** | v0.1 |

## 1. Summary

This RFC defines the **Holon Protocol** for v0.1: a stable, tool-agnostic way to describe a task (**Spec**) and collect results (**Artifacts**) from a headless agent execution inside a sandbox.

This document is intended to be **normative** (what hosts/adapters MUST do). Design notes and reference implementations live under `docs/` (non-normative), e.g. `docs/holon-architecture.md`.

## 2. Core concepts (terms)

### 2.1 Holon (The Unit)
A Holon is a single execution attempt of a defined engineering task. It has a binary outcome state: `Success`, `Failure`, or `NeedsHumanReview`.

### 2.2 Holon Spec (The Input)
A declarative document (YAML/JSON) defining the **Goal** and expected **Artifacts**. Context can include environment variables and a list of relevant files.

### 2.3 Adapter (The Engine)
The container entrypoint that reads Holon inputs and drives an underlying agent/tool runtime headlessly, producing standard artifacts.

### 2.4 Runtime (The Host)
The supervisor (typically the `holon` CLI) that prepares inputs, runs the adapter container, validates artifacts, and hands results to callers/workflows.

## 3. Spec schema (v1)

The `spec.yaml` defines the task. Fields are intended to be forward-compatible; unknown fields SHOULD be ignored by adapters.

```yaml
version: "v1"
kind: Holon

metadata:
  name: "task-name"        # Human-readable slug
  id: "uuid-optional"      # Tracking ID

# INPUT: The context provided to the Agent
context:
  workspace: "/holon/workspace"  # Optional. Default is /holon/workspace.
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

Holon uses a standardized container layout rooted at `/holon`:

### 4.1 Workspace
- `/holon/workspace`: workspace snapshot root (host sets container `WorkingDir` to this path)

### 4.2 Inputs
Hosts MUST mount:
- `/holon/input/spec.yaml` (read-only): the Holon spec.
- `/holon/input/context/` (read-only, optional): injected context files.

Hosts MAY mount:
- `/holon/input/prompts/system.md` (read-only): compiled system prompt.
- `/holon/input/prompts/user.md` (read-only): compiled user prompt.

Secrets MUST be injected via environment variables (not in spec).

### 4.3 Outputs
Adapters MUST write all outputs under:
- `/holon/output/` (read-write): artifacts such as `manifest.json`, `diff.patch`, `summary.md`, and optional `evidence/`.

The Host SHOULD treat `/holon/output/` as the integration boundary and SHOULD ensure it starts empty for each run to avoid cross-run contamination.

## 5. Execution lifecycle (Normative)

A Holon execution is **single-shot**:
- the adapter process starts, performs the task, writes artifacts, and terminates,
- the adapter MUST NOT require inbound ports or run as a daemon.

Exit codes and artifact requirements are defined in `rfc/0002-adapter-scheme.md`.

## 6. Security & network (v0.1)

v0.1 assumes network access is available for calling LLM APIs. Future versions may add stricter egress controls.

## 7. Non-normative references

Design notes and examples (non-normative):
- Architecture/design: `docs/holon-architecture.md`
- Adapter pattern (non-normative): `docs/adapter-encapsulation.md`
- `mode` design (execute/plan/review): `docs/modes.md`
