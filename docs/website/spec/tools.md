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

> **Last verified:** 2026-05-27 against `src/types.rs`
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
| `CoreAgent` | `WaitFor`, `GetAgent`, `SendAgentMessage`, `Enqueue`, `CreateTimer`, `ListTimers`, `GetTimer`, `CancelTimer`, `ListTasks`, `TaskStatus`, `TaskInput`, `TaskOutput`, `TaskStop`, `ListModelProviders`, `ListProviderModels`, WorkItem tools, `MemorySearch`, `MemoryGet` | All agent profiles |
| `LocalEnvironment` | `ExecCommand`, `ExecCommandBatch`, `ApplyPatch`, `ViewImage`, `GenerateImage`, `GetWorkspaceState`, `SwitchWorkspace`, `CreateWorktree` | All profiles |
| `AuthorityExpanding` | `AttachWorkspace`, `DetachWorkspace`, `RemoveWorktree` | Public named agents |
| `Web` | `WebFetch`, `WebSearch`, `XSearch` | All profiles |
| `AgentCreation` | `CreateAgent`, `InvokeAgent` | Public named agents |
| `ExternalTrigger` | `CreateExternalTrigger`, `CancelExternalTrigger` | All profiles |

Operator notification records, delivery callbacks, and UI rendering remain
runtime-owned capabilities. They are not part of the model-facing tool
inventory; `NotifyOperator` is intentionally absent from the built-in tool
registry and machine-readable schema inventory. The `ExternalTrigger` tools
follow the same pattern today: they are dispatchable in the registry but kept
out of the model-facing surface.

## Complete tool listing

### WorkItem plane

| Tool | Purpose |
|------|---------|
| `CreateWorkItem` | Create a new open WorkItem |
| `UpdateWorkItem` | Mutate objective, plan_status, todo_list |
| `PickWorkItem` | Set current focus |
| `GetWorkItem` | Read single WorkItem with plan preview |
| `ListWorkItems` | Query with filters |
| `CompleteWorkItem` | Complete an owned target by ID; promote same-round assistant text as its canonical completion report |
| `WaitFor` | Record task, external, or operator waiting state and yield |

### Task control plane

| Tool | Purpose |
|------|---------|
| `ExecCommand` | Start a shell command |
| `ExecCommandBatch` | Run bounded sequential command batch |
| `ListTasks` | Compact active-task digest with bounded output |
| `TaskStatus` | Single-task lifecycle snapshot |
| `TaskOutput` | Bounded output preview with optional blocking |
| `TaskInput` | Send input to interactive task |
| `TaskStop` | Stop a running task |

### Agent plane

| Tool | Purpose |
|------|---------|
| `GetAgent` | Read current agent-plane summary |
| `WaitFor` | Signal turn-end after recording explicit wait state |
| `Enqueue` | Schedule self-follow-up message |
| `CreateAgent` | Create a long-lived, addressable agent |
| `SendAgentMessage` | Durably send an asynchronous message to an authorized agent |
| `InvokeAgent` | Send and wait for an existing-agent message, or create a result-bearing supervised child |

### Timer plane

| Tool | Purpose |
|------|---------|
| `CreateTimer` | Create an independent or repeating timer |
| `ListTimers` | List this agent's recent timers |
| `GetTimer` | Read one timer by id |
| `CancelTimer` | Cancel an active timer |

### Model plane

| Tool | Purpose |
|------|---------|
| `ListModelProviders` | List configured or discovered model providers |
| `ListProviderModels` | List selectable models for a provider |

### Image plane

| Tool | Purpose |
|------|---------|
| `GenerateImage` | Generate one image from a text prompt |
| `ViewImage` | Validate a local image and record its metadata |

### Workspace plane

| Tool | Purpose |
|------|---------|
| `GetWorkspaceState` | Read bindings, active projection, worktrees, and occupancy |
| `AttachWorkspace` | Attach a workspace binding without switching |
| `DetachWorkspace` | Detach a binding; active targets fall back to agent home |
| `SwitchWorkspace` | Activate an existing workspace or execution root |
| `CreateWorktree` | Create or safely reuse a linked worktree |
| `RemoveWorktree` | Safely remove a registered clean worktree |
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
| `XSearch` | Search public X posts |

## Tool definition contract

Each tool is defined by a `BuiltinToolDefinition`:

```text
BuiltinToolDefinition {
    family: ToolCapabilityFamily,
    spec: ToolSpec { name, description, input_schema, freeform_grammar },
}
```

**Key contract:**

- Tool schemas must match the runtime type used for argument parsing.
- `serde(deny_unknown_fields)` is enforced on tool argument structs.
- The `description` field in `ToolSpec` is the model-visible guidance text.
- Prompt-level tool guidance (in AGENTS.md or system prompt) must not
  contradict the tool's own description.

## Command output safety

`max_output_tokens` controls only the model-visible projection of command
output. Managed command tasks separately enforce a finite combined artifact
retention limit, a higher emitted-byte execution quota, and a filesystem
free-space waterline.

Retention overflow keeps a bounded raw-byte head and tail and continues
draining stdout/stderr. Execution quota, low disk, and persistence failures are
typed terminal failures. `TaskStatus` and `TaskOutput` expose the frozen policy,
byte counters, dropped-byte evidence, and capture truncation through
`output_capture`; `TaskOutput.output_truncated` remains the independent API
preview-truncation flag.

See [Command Task Output Safety](../../rfcs/command-task-output-safety.md).

## Input/result separation

Holon strictly separates tool **startup input** from **result metadata**:

- Startup input: `cmd`, `workdir`, `shell`, `login`, `tty`,
  `duplicate_policy`, `accepts_input`, `yield_time_ms`, `max_output_tokens`.
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

`ExecCommand` results carry additional fields: `disposition`, `exit_status`,
`initial_output_preview`, and `task_handle` (when promoted to command_task).

## Known gaps

- Tool description text is hand-maintained in Rust source; drift between
  description and actual behavior is possible without automated validation.
- `ExecCommandBatch` and individual `ExecCommand` calls share fields but
  have different valid field subsets; no structural schema prevents passing
  batch-only fields to single `ExecCommand` at the type level.
