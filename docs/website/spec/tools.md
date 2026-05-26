---
title: Tools
summary: Current model-facing tool families, authority boundaries, input/result contracts, and deprecated surfaces.
order: 60
---

# Tools

This page defines the current contract for Holon's model-facing tool surface:
tool families, authority classification, schema/dispatch alignment, and
result envelope conventions.

The machine-readable inventory for built-in model-facing tools is
[`model-tool-schema-inventory.json`](../reference/model-tool-schema-inventory.json);
its versioning policy and refresh workflow are documented in the
[model tool schema inventory reference](../reference/model-tool-schema-inventory.md).

> **Last verified:** 2026-05-25 against `src/types.rs`
> `ToolCapabilityFamily`, `src/tool/tools/mod.rs` `builtin_tool_definitions()`,
> `src/tool/spec.rs`, `src/tool/dispatch.rs`.

## Source RFCs

- [Tool Surface Layering](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-surface-layering.md)
- [Tool Contract Consistency](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-contract-consistency.md)
- [Tool Result Envelope](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-result-envelope.md)
- [Task Surface Narrowing](https://github.com/holon-run/holon/blob/main/docs/rfcs/task-surface-narrowing.md)
- [Command Tool Family](https://github.com/holon-run/holon/blob/main/docs/rfcs/command-tool-family.md)
- [Apply Patch Unified Diff Contract](https://github.com/holon-run/holon/blob/main/docs/rfcs/apply-patch-unified-diff-contract.md)
- [Exec Command Batch](https://github.com/holon-run/holon/blob/main/docs/rfcs/exec-command-batch.md)

## Tool families (`ToolCapabilityFamily`)

Tools are grouped by capability family for authority gating:

| Family | Tools | Authority |
|--------|-------|-----------|
| `CoreAgent` | `Sleep`, `WaitFor`, `AgentGet`, `Enqueue`, WorkItem tools, `MemorySearch`, `MemoryGet` | All agent profiles |
| `LocalEnvironment` | `ExecCommand`, `ExecCommandBatch`, `ApplyPatch`, `UseWorkspace` | All profiles |
| `Web` | `WebFetch`, `WebSearch` | All profiles |
| `AgentCreation` | `SpawnAgent` | All profiles |
| `OperatorNotification` | `NotifyOperator` | All profiles |

## Complete tool listing

### WorkItem plane

| Tool | Purpose |
|------|---------|
| `CreateWorkItem` | Create a new open WorkItem |
| `UpdateWorkItem` | Mutate objective, plan_status, todo_list |
| `PickWorkItem` | Set current focus |
| `GetWorkItem` | Read single WorkItem with plan preview |
| `ListWorkItems` | Query with filters |
| `CompleteWorkItem` | Mark complete; promote same-round assistant text as the terminal completion report |
| `WaitFor` | Record task, external, or operator waiting state and yield |

### Task control plane

| Tool | Purpose |
|------|---------|
| `ExecCommand` | Start a shell command |
| `ExecCommandBatch` | Run bounded sequential command batch |
| `TaskList` | Compact active-task digest |
| `TaskStatus` | Single-task lifecycle snapshot |
| `TaskOutput` | Bounded output preview with optional blocking |
| `TaskInput` | Send input to interactive task |
| `TaskStop` | Stop a running task |

### Agent plane

| Tool | Purpose |
|------|---------|
| `AgentGet` | Read current agent-plane summary |
| `Sleep` | Signal turn-end; let scheduler decide next action |
| `WaitFor` | Signal turn-end after recording explicit wait state |
| `Enqueue` | Schedule self-follow-up message |
| `SpawnAgent` | Delegate work to a child agent |

### Workspace plane

| Tool | Purpose |
|------|---------|
| `UseWorkspace` | Switch active workspace |
| `ApplyPatch` | Apply unified diff patch to files |

### Memory plane

| Tool | Purpose |
|------|---------|
| `MemorySearch` | Search agent memory sources |
| `MemoryGet` | Fetch exact memory content by source_ref |

### Web plane

| Tool | Purpose |
|------|---------|
| `WebFetch` | Fetch HTTP/HTTPS URL |
| `WebSearch` | Web search |

## Tool definition contract

Each tool is defined by a `BuiltinToolDefinition`:

```text
BuiltinToolDefinition {
    family: ToolCapabilityFamily,
    spec: ToolSpec { name, description, parameters },
}
```

**Key contract:**

- Tool schemas must match the runtime type used for argument parsing.
- `serde(deny_unknown_fields)` is enforced on tool argument structs.
- The `description` field in `ToolSpec` is the model-visible guidance text.
- Prompt-level tool guidance (in AGENTS.md or system prompt) must not
  contradict the tool's own description.

## Input/result separation

Holon strictly separates tool **startup input** from **result metadata**:

- Startup input: `cmd`, `workdir`, `shell`, `login`, `tty`,
  `accepts_input`, `yield_time_ms`, `max_output_tokens`.
- Result metadata (not valid in startup input): `status`, `task_handle`,
  `disposition`, `exit_status`, `output_preview`.

**Key contract:**

- Passing result fields as startup input is an error.
- The model must not confuse the two surfaces. Prompt guidance explicitly
  documents the distinction via valid/invalid startup examples.

## Result envelope

Tool execution returns a `ToolResult` that may be serialized as JSON or
rendered as a human-readable receipt:

- **Canonical result:** structured JSON with `content` (array of
  text/tool_use blocks) and optional `artifacts`.
- **Human-readable receipt:** rendered text shown to the model; may omit
  internal fields but must preserve semantically meaningful content.

`ExecCommand` results carry additional fields: `disposition`,
`initial_output_preview`, `task_handle` (when promoted to command_task).

## Known gaps

- Tool description text is hand-maintained in Rust source; drift between
  description and actual behavior is possible without automated validation.
- `ExecCommandBatch` and individual `ExecCommand` calls share fields but
  have different valid field subsets; no structural schema prevents passing
  batch-only fields to single `ExecCommand` at the type level.
