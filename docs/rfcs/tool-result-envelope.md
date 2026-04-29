---
title: RFC: Tool Result Envelope
date: 2026-04-23
status: draft
---

# RFC: Tool Result Envelope

## Summary

Holon should treat tool results as a dual-surface contract.

Every built-in tool result should have:

- one canonical structured result owned by the runtime
- one separate model-visible rendering used when the result is fed back to the
  model in tool history

The canonical structured result should keep one shared outer envelope for both
successful and failed tool calls. Tool families may still define their own
typed `result` payloads inside that envelope.

The model-visible rendering is a separate contract. It is not required to be
JSON, and different tool families may define different render conventions when
that improves model readability and continuation quality.

This RFC is about the semantic contract Holon owns across those two surfaces. It
is not about provider-specific wire formats such as OpenAI
`function_call_output` or Anthropic `tool_result`.

## Problem

Holon currently has a shared runtime `ToolResult` container, and tool failures
already use a structured `ToolError`. However, the runtime still treats
serialized canonical JSON as if it were the same thing as model-visible tool
output.

That creates several problems:

- canonical runtime semantics and model-facing readability are coupled together
- provider transports currently pass through serialized envelopes instead of a
  deliberate model-facing rendering
- context compaction cannot cleanly distinguish runtime truth from the receipt
  form shown back to the model
- command-family results become awkward for continuation because pretty JSON is
  heavier and less readable than shell-style receipts
- command output that is itself JSON becomes double-encoded noise when preview
  fields are serialized as escaped JSON strings inside an outer JSON envelope

The immediate pressure comes from command and task output. Large command
results can dominate a single tool loop. Without a stable canonical contract and
a separate model-facing rendering contract, Holon can only choose between two
bad options:

- keep tool output structured, but feed noisy serialized JSON back into the
  model
- optimize for model readability ad hoc, but lose a stable runtime-owned result
  shape

## Goals

- make every built-in tool result self-describing
- use the same canonical outer envelope for success and error
- make model-facing results easier to chain and easier for the model to read
- give context compaction deterministic fields to preserve, trim, or drop
- keep provider transports thin and avoid provider-specific semantic drift
- let each tool family retain its own typed `result` shape
- let each tool family define a stable model-visible rendering convention

## Non-goals

- do not make every tool's inner `result` payload identical
- do not require provider APIs to expose the same fields natively
- do not remove `ToolError`; it should become the `error` payload
- do not solve long-horizon memory compaction by this envelope alone
- do not require one large migration PR for every existing tool
- do not require every tool family to use JSON as its model-visible rendering

## Dual-Surface Contract

Holon should distinguish two forms of tool result:

- canonical structured result: the runtime-owned semantic record used for
  execution, audit, storage, tests, operator/debug views, and compaction
- model-visible rendering: the receipt shown back to the model in tool history
  for follow-up reasoning

These forms are related, but they are not the same contract.

The canonical structured result should be the source of truth. The
model-visible rendering should be derived from it.

`serde::Serialize` is therefore not the model-facing contract by itself.
Serializing the canonical result to JSON is a storage/debug mechanism, not a
definition of what the model should see.

## Canonical Envelope Shape

Every built-in tool should produce a canonical structured result with one shared
outer envelope.

The canonical envelope should have this shape:

```json
{
  "tool_name": "ExecCommand",
  "status": "success",
  "summary_text": "command exited with status 0",
  "result": {},
  "error": null
}
```

The same canonical envelope should be used for errors:

```json
{
  "tool_name": "ExecCommand",
  "status": "error",
  "summary_text": "input for ExecCommand does not match the tool schema",
  "result": null,
  "error": {
    "kind": "invalid_tool_input",
    "message": "input for ExecCommand does not match the tool schema",
    "details": {
      "tool_name": "ExecCommand",
      "parse_error": "missing field `cmd`"
    },
    "recovery_hint": "provide input for ExecCommand that matches the published tool schema",
    "retryable": false
  }
}
```

This canonical envelope is the shared semantic record Holon keeps internally.
It is not, by itself, a requirement that the model-visible rendering must be
JSON.

## Canonical Field Semantics

### `tool_name`

The public tool name that produced this result.

This should be the model-facing tool name, not an internal runtime type.

### `status`

The high-level execution status.

Allowed first-pass values:

- `success`
- `error`

Holon should not require the model to infer failure from provider-native
`is_error` flags or from parsing prose.

Provider transports may still map this to native fields when available. The
canonical envelope remains the runtime semantic record.

### `summary_text`

A short human-readable summary suitable for:

- agent follow-up reasoning
- transcript summaries
- context compaction
- operator-facing debug views

This field should be present for both success and error.

It should be concise. Large stdout, stderr, task logs, or full child-agent
reports should not be placed here.

### `result`

The successful typed result payload.

Each tool family owns the inner shape. For example:

- command tools may return command disposition, exit status, output preview,
  truncation flags, and task handles
- task tools may return task snapshots or output snapshots
- file tools may return changed paths and receipt metadata
- workspace tools may return active workspace / projection metadata
- work-item tools may return updated work queue snapshots

When `status = "error"`, `result` should be `null`.

### `error`

The shared `ToolError` payload.

When `status = "success"`, `error` should be `null`.

When `status = "error"`, `error` should contain:

- `kind`
- `message`
- optional `details`
- optional `recovery_hint`
- `retryable`

This replaces the older model where the error envelope was rendered directly as
the whole tool output string.

## Model-Visible Rendering

The model-visible rendering is the form returned to the provider transport as
tool output for continuation context.

It should be derived from the canonical result, but it is a separate contract.

The model-visible rendering:

- is family-defined rather than globally hard-coded to JSON
- should optimize for model readability and continuation quality
- should remain stable enough for tests and deterministic prompt behavior
- should avoid nested escaped JSON or other high-noise formatting when a better
  receipt form exists

Different tool families may choose different render conventions when the choice
materially improves model reasoning.

For example:

- command family tools may render as concise textual receipts
- task-family output reads may render as compact textual receipts with status,
  handles, preview text, and artifact references
- small receipt-style tools may render as short textual confirmations
- some tools may still choose JSON-like rendering when structure is the most
  readable form

The key boundary is that the runtime should not treat raw JSON serialization of
the canonical envelope as the default model-facing rendering contract.

## Complete Output And Projection

Holon should distinguish three forms:

- complete tool output: the full runtime-owned result, including any large
  family-specific payloads and durable artifact records
- canonical structured result: the stable structured envelope Holon uses as its
  normal semantic record
- model-visible rendering: the bounded receipt derived for provider tool-result
  history

Tools should produce complete typed outputs. They should not hand-roll the final
model-visible receipt string.

The runtime should expose deterministic projection methods for both surfaces:

```rust
fn to_canonical_result(
    full: CompleteToolOutput,
    policy: ToolOutputProjectionPolicy,
    artifacts: &mut dyn ToolArtifactSink,
) -> CanonicalToolResult

fn render_for_model(
    canonical: &CanonicalToolResult,
    policy: &ToolOutputRenderPolicy,
) -> String
```

The exact Rust names may differ, but the contract should stay the same:

1. validate the complete output
2. project the family-specific full result into a bounded canonical `result`
3. write large full payloads to artifacts when needed
4. set family-specific truncation fields and artifact indices
5. render the canonical result into a model-visible receipt using the
   appropriate tool-family convention

### Complete Output Shape

The complete output should carry full runtime data, not only the prompt-sized
projection:

```rust
struct CompleteToolOutput {
    tool_name: String,
    status: ToolOutputStatus,
    summary_text: String,
    result: Option<CompleteToolResult>,
    error: Option<ToolError>,
}
```

`CompleteToolResult` should be typed by tool family. For example, command
execution may keep complete stdout and stderr as structured fields, while file
mutation tools may keep changed paths and receipt metadata.

The complete output can be persisted in runtime storage or backed by existing
task/output files. The canonical result and model-visible rendering are both
derived surfaces over that full output.

### Canonical Projection Is Typed, Not Blind JSON Truncation

The projection method should not blindly truncate arbitrary serialized JSON.

Instead, each tool family should own a small projector for its complete result
type:

```rust
trait ToolResultProjector {
    fn project(
        &self,
        policy: &ToolOutputProjectionPolicy,
        artifacts: &mut dyn ToolArtifactSink,
    ) -> ProjectedToolResult;
}
```

The shared runtime owns the canonical outer envelope and error handling. The
family projector owns the inner `result` payload.

This keeps the truncation automatic for tool authors while preserving semantic
control over large fields.

### Model Rendering Is Family-Defined

The model-visible rendering should not be one blind serialization step over the
canonical result.

Instead, Holon should let each tool family define a stable rendering convention
for the model-facing receipt.

The preferred abstraction is a renderer contract such as:

```rust
trait ToolResultModelRenderer {
    fn render_for_model(
        &self,
        canonical: &CanonicalToolResult,
        policy: &ToolOutputRenderPolicy,
    ) -> String;
}
```

The exact Rust names may differ, but the boundary should remain:

- canonical structured result is runtime-owned truth
- family renderers define what the model sees
- provider transports consume the rendered receipt, not raw serialized canonical
  JSON

### Projection Algorithm

The default overall projection algorithm should be:

1. Start from one complete tool output.
2. Validate success/error exclusivity:
   - `status = success` requires `result`
   - `status = error` requires `error`
3. For success, call the family projector to create a bounded canonical
   `result`.
4. For error, project `ToolError` into the canonical error payload while
   preserving stable error fields.
5. Whenever a large field exceeds its family budget:
   - write or reference the full value through `ToolArtifactSink`
   - record a path-only artifact reference in the family result
   - replace the large field with a bounded preview or omit it
   - add a family-specific artifact index in `result`
   - set the family-specific truncation flag
6. Pass the canonical result to the family model renderer.
7. Return both:
   - the canonical structured result
   - the model-visible rendered receipt

Both projection stages should be deterministic for the same complete output,
policy, and artifact sink state.

### Artifact Index Mapping

Artifact references should stay path-only:

```json
{
  "path": "/path/to/output.log"
}
```

When a tool needs to explain which artifact is stdout, stderr, a failure log, or
another family-specific role, the role should be expressed in `result` by
referencing the artifact index.

For example:

```json
{
  "result": {
    "stdout_preview": "...",
    "stderr_preview": "...",
    "stdout_truncated": true,
    "stderr_truncated": true,
    "stdout_artifact": 0,
    "stderr_artifact": 1
  },
  "artifacts": [
    { "path": "/path/to/stdout.log" },
    { "path": "/path/to/stderr.log" }
  ]
}
```

If there is only one combined output artifact, the family result can reference
that single index:

```json
{
  "result": {
    "combined_output_preview": "...",
    "combined_output_truncated": true,
    "combined_output_artifact": 0
  },
  "artifacts": [
    { "path": "/path/to/output.log" }
  ]
}
```

The meaning of an artifact should never depend on array position alone when
multiple artifacts exist.

## Family-Specific Large Output Rules

Holon should treat truncation as a first-class part of high-volume tool-family
result contracts, not as an incidental string suffix.

The initial pressure comes from the command family, especially:

- `ExecCommand`
- `TaskOutput`
- related command-backed task inspection surfaces where large output may re-enter
  model context

These families should define explicit bounded preview and truncation fields
inside their typed `result` payloads.

For example, a command-family result may include:

```json
{
  "stdout_preview": "...",
  "stderr_preview": "...",
  "truncated": true,
  "artifacts": [
    { "path": "/path/to/stdout.log" },
    { "path": "/path/to/stderr.log" }
  ],
  "stdout_artifact": 0,
  "stderr_artifact": 1
}
```

Other tool families should add truncation metadata only when they actually have
high-volume payloads that need bounded canonical projection.

Small receipt-style tools should not be forced to carry no-op truncation or
artifact fields.

### Preserve The Stable Canonical Envelope

Large-output handling must not remove or rename the outer envelope fields.

Even when a family-specific canonical result is truncated, Holon should
preserve:

- `tool_name`
- `status`
- `summary_text`
- `error`

For errors, Holon should also preserve:

- `error.kind`
- `error.message`
- `error.recovery_hint`
- `error.retryable`

If `error.details` is too large, Holon may truncate or summarize only
`error.details`. That does not require a new top-level truncation field.

### Prefer Head And Tail For Logs

For log-like payloads such as stdout, stderr, test output, and task logs, Holon
should prefer a head-and-tail preview rather than only keeping the prefix.

The preview should make the cut explicit, for example:

```text
<first lines>
...
[output truncated: showing first N and last M lines]
...
<last lines>
```

This helps the agent see both command setup and final failure context.

### Prefer Textual Receipts For Command-Family Rendering

The command family should prefer concise textual receipts when rendered back to
the model.

This especially applies to:

- `ExecCommand`
- `TaskOutput`

For these tools, a readable receipt is usually more useful for continuation than
raw serialized JSON. The rendered form should expose the same semantic signals
from the canonical result, but in a format closer to shell/task receipts, for
example:

```text
Process exited with code 0

stdout:
src/runtime/turn.rs
src/runtime/lifecycle.rs
```

or:

```text
Command promoted to background task
Task: task_123

Initial output:
Starting server on :3000
```

If stdout or stderr is itself JSON, the rendered receipt should prefer readable
pretty text over escaped JSON nested inside an outer serialized envelope.

### Prefer Concise Receipts For Small Confirmation Tools

Small confirmation-style tools should generally prefer concise textual receipts
over verbose canonical dumps when rendered back to the model.

Examples include:

- file mutation receipts
- work-item mutation receipts
- workspace entry/exit receipts
- notification receipts

These tools should surface the key confirmation and identifiers the model needs
for the next step, without forcing the model to parse low-value structured
noise.

### Prefer Structured Canonical Previews Over Raw Blobs

Tool families should avoid returning large raw strings directly when a
structured canonical preview is possible.

For command output, prefer fields such as:

```json
{
  "stdout_preview": "...",
  "stderr_preview": "...",
  "truncated": true,
  "stdout_artifact": 0
}
```

over one unstructured `output` blob.

### Do Not Duplicate Large Payloads

When a full payload is available through an artifact reference, Holon should not
also duplicate it into both `result` and `artifacts`.

The intended pattern is:

- `summary_text`: concise result
- `result`: bounded structured preview, handles, and any artifact references

This rule is especially important for `TaskOutput`, where output and result
summary can otherwise repeat the same large text.

### Truncation Should Be Stable Across Re-serialization

Once Holon has returned a truncated tool envelope to the model, later prompt
construction and context compaction should not accidentally re-expand it from
runtime storage.

Runtime storage may keep full artifacts, but model-visible projections should
remain bounded unless the agent explicitly asks for a narrower slice through an
appropriate tool.

### Budget Ownership

Each tool family should own its default model-visible budget for its inner
`result` payload. The shared envelope owns only the invariant that result
payloads should remain bounded and semantically self-describing.

Suggested first-pass ownership:

- command tools own stdout/stderr preview budgets
- task output owns task-output preview budgets
- child-agent supervision owns child-result report budgets
- file mutation tools own changed-path and receipt budgets
- the shared envelope owns error-envelope and metadata preservation rules

### Artifact References In Family Results

Optional artifact references may appear inside family-specific result payloads
when large full data should not be inlined into the active model context.

Examples:

- command output file paths
- failure artifact paths
- generated patch or report paths
- durable task logs

The first-pass shape should stay small:

```json
{
  "path": "/path/to/output.log"
}
```

Artifact references are references, not a second place to duplicate large
payloads.

Artifact type and provenance should be implicit in the containing tool result
envelope, the family-specific `result` fields, and the runtime transcript
record. The model-visible artifact reference should not repeat provenance or
description fields unless the artifact is later projected outside the original
tool result.

If an artifact must be shown independently in a future prompt projection, the
renderer should derive any human-readable description from the runtime record
instead of storing a second model-visible description inside the artifact
reference.

## Command Tool Canonical Example

An immediate `ExecCommand` success should look like:

```json
{
  "tool_name": "ExecCommand",
  "status": "success",
  "summary_text": "command exited with status 0",
  "result": {
    "disposition": "completed",
    "exit_status": 0,
    "stdout_preview": "short output preview",
    "stderr_preview": null,
    "truncated": false
  },
  "error": null
}
```

A promoted command should look like:

```json
{
  "tool_name": "ExecCommand",
  "status": "success",
  "summary_text": "command promoted to managed task",
  "result": {
    "disposition": "promoted_to_task",
    "task_handle": {
      "task_id": "task_123",
      "kind": "command_task"
    },
    "initial_output_preview": "short output preview",
    "initial_output_truncated": false
  },
  "error": null
}
```

`TaskOutput` should also use the shared outer envelope. Its `result` may contain
`retrieval_status` and a task output snapshot, but large output should be
bounded and should reference artifacts when available.

## Error Example

An execution-root violation should look like:

```json
{
  "tool_name": "ExecCommand",
  "status": "error",
  "summary_text": "requested working directory is outside the current execution root",
  "result": null,
  "error": {
    "kind": "execution_root_violation",
    "message": "requested working directory is outside the current execution root",
    "details": {
      "workdir": "../other-repo"
    },
    "recovery_hint": "omit workdir or use a relative path inside the active workspace",
    "retryable": false
  }
}
```

## Command Tool Rendered Example

The command-family model-visible rendering should usually be a textual receipt
derived from the canonical result rather than the raw serialized envelope.

For example, an immediate `ExecCommand` success may render as:

```text
Process exited with code 0

stdout:
short output preview
```

A promoted command may render as:

```text
Command promoted to background task
Task: task_123

Initial output:
short output preview
```

The exact wording may evolve, but the family contract should stay stable:

- expose the key outcome first
- keep task handles and artifact references readable
- prefer readable preview text over nested escaped JSON

## Provider Transport Contract

Provider transports should preserve the canonical result and the model-visible
rendering as separate concepts.

That means:

- provider transports consume the rendered receipt as the model-visible tool
  output
- provider transports may still map canonical error state to native flags such
  as Anthropic `is_error`
- provider-native flags are optimizations or compatibility aids, not the
  canonical Holon contract
- the runtime should not require the model to understand different semantic
  result shapes depending on provider

The provider-facing tool-result string may differ from the canonical envelope's
JSON serialization as long as both are derived from the same canonical result.

## Compaction Contract

The canonical result gives deterministic compaction rules:

- always preserve `tool_name`
- always preserve `status`
- always preserve `summary_text`
- always preserve `error.kind`, `error.message`, `error.recovery_hint`, and
  `error.retryable`
- preserve compact IDs and handles such as `task_handle.task_id`
- trim or drop large preview fields inside `result`
- preserve family-specific artifact references instead of inline artifact
  content

This is especially important for in-turn tool-loop compaction. A compactor
should not have to parse arbitrary pretty JSON or prose to know which fields are
safe to keep.

Compaction may later derive a bounded model-visible receipt from the preserved
canonical result, but compaction should not depend on re-parsing provider-facing
receipt text to recover runtime semantics.

## Runtime Storage Contract

Holon may continue storing richer internal records, including:

- original tool input
- structured runtime output
- audit-only details
- latency and trust metadata
- full output artifacts

The canonical structured result is the stable runtime record. The model-visible
rendering is a derived receipt for prompt history, not the sole source of
truth.

Audit records should preserve both:

- the canonical result
- the rendered receipt returned to the model
- structured runtime-only details where useful

## Relationship To Other RFCs

- `tool-contract-consistency.md` defines naming, schema, and family-level
  consistency rules.
- `tool-surface-layering.md` defines capability families and stable tool
  visibility.
- `command-tool-family.md` defines command-family semantics. Command-family
  outputs should use the canonical envelope and define both their canonical
  `result` payload and their family-specific model-visible rendering rules.
- `long-lived-context-memory.md` defines broader memory and compaction goals.
  This RFC gives compaction a stable canonical tool-result contract to operate
  on while keeping provider-facing receipts separately optimized for the model.

## Open Questions

- Should `status` later distinguish `cancelled`, `timeout`, or `not_ready`, or
  should those remain family-specific values inside `result`?
- Should independently projected artifact references need a richer derived view,
  or should the model-visible reference remain path-only everywhere?
- Should external dynamic tools be normalized into this envelope at the runtime
  boundary, or should the rule initially apply only to Holon built-ins?

## Summary

Holon should stop treating canonical structured tool results and model-visible
tool receipts as the same thing.

The canonical structured result gives Holon one predictable semantic contract
for runtime behavior, storage, tests, and compaction. The separate
model-visible rendering lets each tool family optimize what the model sees for
readability and continuation quality without sacrificing runtime clarity.
