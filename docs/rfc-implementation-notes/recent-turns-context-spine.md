# Recent Turns Context Spine

This note captures the planned `recent_turns` prompt-context improvement as an
implementation follow-up. It belongs here rather than under
`docs/implementation-decisions/` because the work depends on the runtime
database/evidence migration and describes rollout shape, rendering behavior,
and migration fallback rather than one durable architectural decision.

## Implementation order

Implement this after the runtime database storage refactor is stable, including
DB-backed state domains, evidence indexing, and domain cutover hardening.

Until turn, message, brief, delivery, and tool-execution evidence is queryable
from the shared runtime database, the current projected recent-context renderer
should remain the compatibility path.

## Target direction

Model-visible recent context should use `recent_turns` as the primary recent
history spine.

When persisted `TurnRecord` entries are available, `recent_turns` should be
rendered from those records and joined to the referenced messages, briefs, tool
executions, deliveries, waits, and completed work items. When turn records are
missing or incomplete, the renderer may fall back to the existing
message/brief/tool projection so older or partial logs remain recoverable.

The regular recent-context layout should move from parallel full-history
sections:

```text
recent_turns
recent_messages
latest_result
recent_briefs
recent_tool_executions
```

to a turn-centered layout:

```text
recent_turns
recent_tool_alerts
```

`latest_result` should be folded into the relevant turn as the latest delivery
or produced result, not repeated as a separate full section.

## Reason

The older parallel layout duplicates the same recent activity through multiple
projections:

- messages show recent inputs
- briefs show recent outcomes
- tool executions show recent evidence
- latest result repeats the most recent delivery
- `recent_turns` groups parts of the same data again

That duplication consumes prompt budget and can obscure turn boundaries. A
turn-centered projection better matches Holon's durable execution model: a turn
has input messages, tool executions, produced briefs, delivery summaries,
completed work items, waits, and a terminal state.

Using `TurnRecord` as the primary index also makes the section name accurate.
The section is not a loose activity feed; it is the model-visible rendering of
recent runtime turns. Projection fallback remains necessary for migration,
development, failure paths, and older logs that do not yet have complete turn
records.

## Rendering contract

`recent_turns` must preserve the semantic content that the removed sections used
to provide:

- Trusted operator input is authority-bearing task text. It should be rendered
  as original text within budget, not replaced by a model summary. If it must be
  truncated, the truncation should be explicit.
- Runtime continuation input may be summarized, but it should retain trigger,
  provenance, and relation to the trusted operator input or current work item.
- Result, failure, blocked, verification, wait, and delivery briefs should be
  retained with enough detail to recover the latest outcome and decision. Ack
  briefs are runtime lifecycle acknowledgements such as `Queued work: ...`,
  not task outcomes; they are usually low-value and may be omitted.
- Normal successful tool executions should be summarized as traceable evidence:
  status, tool kind, command preview or equivalent request preview, and stable
  command/output references when available.
- Failed, cancelled, promoted/running, truncated, artifact-producing, or
  unmatched tool executions should appear in alert/fallback context even if they
  cannot be attached cleanly to a turn.

## Prompt layout

The current implementation already renders a projected `recent_turns` section
from messages, briefs, and tool executions. Its prompt shape is:

```text
recent_turns:
Recent turns:
- Turn message_seq <n>|<message_id>:
  - trigger: trusted operator input|<runtime continuation label>
  - continues input: message_seq <n>|<message_id>
  - continuation trigger: <runtime continuation label>
  - operator asked: <original operator input preview>
  - input: <non-operator input preview>
  - produced briefs:
    - <BriefKind>: <brief text preview>
  - tool executions:
    - [<authority>][<status>] <summary> tool_execution_id=<id> ...
  - current relation: <runtime continuation label>
  - current input: <current continuation input preview>
  - current work item: <work_item_id> :: <objective preview>
```

Only the fields that apply to a turn are shown. For example, `continues input`,
`continuation trigger`, `current relation`, `current input`, and `current work
item` are only added when the current runtime continuation is being rendered
against the latest trusted operator input. Trusted operator input uses
`operator asked`; other inputs use `input`.

Tool rows are compact evidence handles, not full output dumps. For command
tools they include stable trace fields such as `tool_execution_id`,
`cmd_digest`, `cmd_ref`, and `cmd_preview`; for command batches they include
one such tuple per batch item. The stable refs are the path for retrieving
fuller command evidence outside the prompt budget.

The target `TurnRecord`-first renderer should keep the same operator-facing
shape but use the durable turn record as the join spine. A full turn can be
assembled from these `TurnRecord` fields:

```text
turn_id
turn_index
agent_id
run_id
current_work_item_id
trigger
input_message_ids
tool_execution_ids
produced_brief_ids
delivery_summary_ids
completed_work_item_ids
waiting_condition_ids
terminal
created_at
```

The prompt should not dump those fields mechanically. It should render the
model-useful projection:

```text
- Turn <turn_index>|<turn_id>:
  - trigger: <trigger summary>
  - input:
    - operator asked: <trusted operator text, preserved within budget>
    - continuation/input: <non-authority input preview with provenance>
  - work item: <current_work_item_id> :: <objective preview>
  - tool executions:
    - <compact status and stable evidence refs>
  - produced results:
    - <Result/Failure/Blocked/Verification/Wait/Delivery brief>
  - completed work items:
    - <id> :: <completion preview>
  - waiting:
    - <wait condition preview>
  - terminal: <terminal kind/status, round, duration, or recovery summary>
```

Normal turns may omit empty groups. `latest_result` becomes the newest relevant
`produced results` or delivery entry inside the latest turn instead of a
separate section.

## Budgeting and full-evidence recovery

`recent_turns` should be budgeted as a recent context projection, not as a
lossless transcript.

Recommended trimming rules:

- Preserve trusted operator input as original text within budget. If it is
  shortened, mark truncation explicitly so the model knows it is incomplete.
- Keep the latest turn and any current continuation turn at higher priority
  than older completed turns.
- Retain outcome-bearing briefs: `Result`, failure, blocked, verification,
  wait, delivery, and completion summaries. Drop ordinary `Ack` briefs unless
  they are the only evidence for a state transition. In the current runtime,
  operator-input `Ack` records are produced when the runtime accepts/queues the
  message before model execution; once the same turn also shows the input and
  later result, the ack adds little model value.
- Keep successful tool executions as compact evidence rows. Do not inline long
  command output, screenshots, raw JSON, or artifact contents.
- Prefer dropping older successful tool rows before dropping trusted operator
  input or latest outcome rows.
- Keep failed, cancelled, promoted/running, truncated, artifact-producing, and
  unmatched tools visible via `recent_tool_alerts` if they are omitted from a
  turn body.
- Preserve stable identifiers and refs (`turn_id`, `turn_index`,
  `message_seq`, `tool_execution_id`, `cmd_ref`, artifact refs, work item ids)
  whenever content is clipped.

When the prompt projection is not enough, the agent should retrieve full
evidence from durable runtime state rather than expecting the prompt to carry
everything:

- Use `message_seq`, `turn_id`, or `turn_index` to locate the durable turn,
  message, brief, delivery, wait, and terminal records.
- Use `tool_execution_id` and command refs such as `cmd_ref` to inspect the
  exact tool request/receipt and, when present, output artifacts.
- Use work item ids to read the durable work item, todo list, and plan artifact.
- Use transcript or audit/event storage for recovery/debugging paths when the
  turn projection is incomplete or migration data is inconsistent.

This keeps the model-visible prompt small while preserving a reliable path from
the compact turn row back to the full execution evidence.

## Boundary

This is a prompt projection contract, not a durable storage compaction contract.

Messages, briefs, tool executions, transcripts, and turn records remain durable
runtime evidence. The renderer may omit or compress low-value content from the
provider request, but it must not make prompt omission equivalent to deleting or
rewriting stored evidence.

The reduced recent sections are fallback and alert surfaces:

- tool alerts protect failures, long-running tasks, truncation, artifacts, and
  unmatched execution evidence

This preserves the boundary between durable event logs and bounded
model-visible context while making recent prompt context smaller and more
turn-structured.

## Brief storage boundary

Operator input should not be re-modeled as a brief. It is already durable as a
message/transcript entry and, in the turn-centered model, is referenced through
`TurnRecord.input_message_ids`. Making input another `BriefKind` would duplicate
authority-bearing text and blur the boundary between input evidence and runtime
summaries.

`briefs.jsonl` should primarily store concise runtime summaries that are useful
after a turn or task state transition: result, failure, blocked/waiting,
verification, completion, and other outcome-bearing records. Those records are
not a replacement for transcript messages; they are compact status evidence that
can be attached to turns, work items, or tasks.

The current operator-input `Ack` records are weaker than those outcome briefs:
they acknowledge queue/admission progress before model execution, but they are
not an input, not a result, and usually not useful once the same turn has the
original input and a later terminal result. The preferred direction is:

- keep trusted operator input in message/transcript storage, not briefs;
- keep outcome-bearing summaries in `briefs.jsonl`;
- avoid adding `BriefKind::Input`;
- treat ordinary `Queued work: ...` acknowledgements as lifecycle/admission
  evidence rather than model-facing brief content;
- if acknowledgement evidence is still needed durably, prefer a more explicit
  queue/turn lifecycle event or status record over preserving it as a normal
  produced brief.

Until storage is changed, prompt rendering should continue to omit ordinary
operator-input `Ack` briefs from `recent_turns` when the input and outcome are
already visible, while retaining trace identifiers so the underlying stored
record can still be recovered for debugging or migration.
