---
title: RFC: Work Item Runtime Model
date: 2026-04-18
updated: 2026-05-03
status: draft
---

# RFC: Work Item Runtime Model

## Summary

This RFC defines `WorkItem` as Holon's durable unit for sustained agent work.

The current direction is:

- `WorkItem` is the goal-oriented runtime anchor.
- `objective` is the short current-work target.
- `plan_artifact` points to a durable natural-language plan file, similar to
  the useful artifact produced by Claude Code plan mode.
- `todo_list` is a small structured progress checklist under the plan.
- `blocked_by` is WorkItem-level, not todo-item-level.
- external queues and routing systems stay outside the agent-owned current
  work surface.

Holon should not model this as a strong interactive plan mode. In daemon mode,
the runtime should instead provide tools that let the agent create, refine, and
confirm the same planning artifact asynchronously before implementation.

## Problem

Holon's runtime semantics are anchored at lower layers:

- message ingress
- turn execution
- internal task execution

Those layers are necessary, but they are not sufficient for long-running agent
work. The runtime also needs a stable, transparent unit for:

- the current high-level objective;
- the agent's durable understanding of the plan;
- progress tracking across turns and compaction;
- waiting and blocker state;
- completion and delivery summaries;
- control-plane and TUI visibility.

Recent benchmark failures showed that a short target plus a checklist is too
weak. A generic objective such as "fix issue #862" can outlive the precise
issue interpretation after compaction, and the checklist then starts doing too
much semantic work.

The fix is not to remove WorkItem in favor of a plain todo list. A plain todo
list is useful for progress tracking, but once it gains a durable objective,
plan text, blocker state, waiting identity, and completion state, it has become
the useful part of WorkItem again.

## Work Item vs Turn vs Task

These concepts live at different layers.

### Turn

A `Turn` is the smallest conversational execution unit.

It answers:

- what happened in this round of interaction?

It does not answer:

- what high-level work is currently being pursued?
- whether that high-level work is now complete?

### Task

A `Task` is an operational execution unit inside the runtime.

Tasks exist to perform concrete work, such as:

- a command task;
- a delegated child-agent execution;
- a background runtime job.

A task answers:

- what concrete execution is currently running, waiting, or complete?

It does not define the user-visible high-level work goal.

### WorkItem

A `WorkItem` is the high-level unit of ongoing agent work.

It answers:

- what meaningful piece of work is the agent currently advancing?

A WorkItem may span:

- multiple turns;
- multiple internal tasks;
- multiple pauses, waits, callbacks, and resumptions;
- multiple compaction events.

In short:

- `Turn` is conversational.
- `Task` is operational.
- `WorkItem` is goal-oriented.

## Goals

This RFC aims to let Holon:

- continue work across turns instead of treating each ingress as isolated;
- preserve task understanding through compaction;
- let agents plan before implementation without requiring a synchronous user
  interaction loop;
- accept external ingress without always aborting current work;
- track blockers and completion above individual turns and tasks;
- keep external queue ownership separate from agent-owned current work.

## Non-goals

This RFC does not attempt to:

- model a full external issue backlog inside the runtime;
- define a project-management system;
- require user approval before every implementation;
- define a semantic verifier for whether a plan is correct;
- replace transcript, tool records, briefs, or delivery summaries as evidence.

## Core Model

The minimal WorkItem state is:

- `id`
- `objective`
- `state`
- `plan_status`
- `plan_artifact`
- `todo_list`
- `blocked_by`
- `recheck_at`
- `recheck_consumed_at`
- `result_summary`
- `created_at`
- `updated_at`

### `objective`

`objective` is the short current-work target.

It should say what the agent is trying to accomplish now:

```text
Split compaction provider fixtures into a focused support module
```

It should not be a broad wrapper around process mechanics:

```text
Fix GitHub issue #862 and open a PR
```

`objective` replaces the old delivery-target framing. It is a runtime anchor,
not merely a UI title, but it is still intentionally short. Detailed task
understanding belongs in the WorkItem plan artifact.

### `state`

The WorkItem lifecycle state set is:

- `open`
- `completed`

`open`
- the work item still represents unfinished work.

`completed`
- the objective is done and should no longer drive activation.

Blocked and queued are derived views, not lifecycle states:

- blocked work is `open` work with `blocked_by` set;
- queued work is `open` work that is not the current focus and has no blocker.

Blocked WorkItems may also carry a one-shot fallback recheck deadline:

- `recheck_at` records the next absolute time when the runtime should wake the
  agent to re-evaluate the blocker;
- `recheck_consumed_at` records that the current `recheck_at` reminder has
  already been delivered or consumed;
- clearing `blocked_by` clears both recheck fields;
- a due recheck never makes the WorkItem runnable by itself. The WorkItem stays
  blocked until the agent explicitly refreshes or clears the blocker.

### `plan_status`

`plan_status` records whether the durable plan is ready to guide
implementation.

The initial state set is:

- `draft`
- `ready`
- `needs_input`

`draft`
- the agent is still inspecting the task source and shaping the plan.

`ready`
- the plan is clear enough to implement.

`needs_input`
- the plan cannot be safely finalized without operator or external input.
- the work item remains open but is not scheduler-runnable until the agent
  processes the input and updates the work item back to `ready` or `draft`.

Daemon mode does not require a human to confirm every plan. The agent may move
from `draft` to `ready` when the task boundary is clear. It should use
`needs_input` only for real ambiguity, missing authority, or an external
decision.

### `plan_artifact`

`plan_artifact` is the descriptor for a durable markdown plan file owned by the
WorkItem.

The plan body lives in AgentHome, for example:

```text
agent_home/work-items/<work_item_id>/plan.md
```

AgentHome is the agent's default durable workspace. The plan file is not stored
inside the project workspace and should not be committed with user code.

The WorkItem ledger stores plan metadata, not the full plan body. The first
descriptor version is path-first:

- absolute `path`;
- content hash;
- byte size;
- updated timestamp;
- preview text;
- `preview_complete`.

`path` is the agent-facing locator. It lets the agent read, grep, or shell-edit
the plan regardless of the active workspace. Workspace-relative identity can be
added later after AgentHome has a globally unique workspace id.

The file itself is the WorkItem's stable task-understanding artifact. It may
include:

- task interpretation;
- scope and non-goals;
- implementation strategy;
- verification approach;
- notable risks or assumptions.

The plan should be concise enough to scan and rich enough to prevent objective
drift. It does not need to fit into every model request because agents can read,
grep, and patch the file on demand.

Example:

```markdown
This is a test-support refactor, not a runtime behavior fix.

Split compaction-related provider fixtures out of
tests/support/runtime_providers.rs into a focused support module. Keep existing
runtime compaction tests behavior-preserving.

Do not change OpenAI transport or production runtime compaction logic unless a
test compile failure proves the support split requires it.

Verify with the focused runtime compaction tests and any affected test targets.
```

The plan is not a progress checklist. It is allowed to change, but changing the
plan file means the agent is changing its durable interpretation of the work.

### `todo_list`

`todo_list` is the structured progress checklist under the plan.

Each item contains:

- `text`
- `state`

The item state set is intentionally the Codex-style three-state model:

- `pending`
- `in_progress`
- `completed`

There is no todo-item-level `blocked` state in the first model. If the whole
objective cannot currently advance, the agent records an explicit `WaitFor`.
`blocked_by` remains display text written by that wait path. If a single step
no longer makes sense, the agent should edit the plan artifact or replace the
todo list.

At most one item should normally be `in_progress`.

### `blocked_by`

`blocked_by` is optional WorkItem-level wait/blocker display text.

It means the objective cannot currently be advanced by the agent. Examples:

- waiting for operator input;
- waiting for an external callback;
- waiting for CI or review when no useful same-objective work remains;
- missing authority or unavailable dependency.

If only one step is awkward but the objective can still progress, do not set
`blocked_by`. Update `todo_list` or refine the plan artifact instead.

External waits should be represented through `WaitFor(wake=external)` plus any
external trigger, timer, callback, or inbox subscription needed for liveness.
`blocked_by` should explain the blocker; it should not be the only durable
wake mechanism.

## Plan-Then-Implement Flow

Holon should provide the benefits of Codex and Claude Code plan mode without
requiring a synchronous mode switch.

The runtime should support this daemon-friendly flow:

1. create or pick a WorkItem with a short `objective`;
2. inspect the source task, code, local docs, and relevant external context;
3. edit the durable WorkItem plan artifact and set `plan_status`;
4. maintain a `todo_list` as execution progress;
5. implement once the plan is `ready`;
6. if the implementation needs to change the task interpretation, edit the plan
   artifact before continuing;
7. mark the WorkItem completed when the objective is satisfied.

This is a tool protocol, not a UI-only state. A TUI can render "planning" or
"implementing", but the runtime source of truth is the WorkItem state.

## Ingress And Work-Item Resolution

New ingress does not automatically become a new WorkItem.

Ingress first enters the runtime as input. It then affects WorkItems through
explicit resolution:

- update the current WorkItem;
- create a new WorkItem;
- update an existing open or blocked WorkItem;
- remain informational only.

The boundary is the `objective`, interpreted through the plan artifact.

If newly discovered work is required to complete the same objective, it should
stay inside the current WorkItem and edit the plan artifact or update the todo
list.

If newly discovered work forms a different objective, it may become a different
WorkItem or be handed to an external queue/backlog.

Agents should not create new WorkItems merely to narrow the current task after
inspection. They should update `objective`, edit the plan artifact, and update
`todo_list` on the same WorkItem.

## Work Queue And Focus

The work queue is the runtime container for WorkItems known to this agent.

It is not:

- a raw message queue;
- a transcript index;
- an internal task scheduler;
- a full external issue backlog.

The agent has one explicit focus pointer:

- `current_work_item_id`

If `current_work_item_id` points to an open WorkItem, that item is the current
work for the agent.

The initial scheduling model allows only one current WorkItem per agent. This
does not forbid multiple internal tasks or delegated child agents. It only
means the top-level agent-owned objective is singular.

## Activation And Tick Behavior

Tick should ask:

- is there runnable work worth activating?

Runnable is a derived view, not a stored lifecycle state:

- `state = open`
- no `blocked_by`
- `plan_status != needs_input`

The minimal rule is:

1. if the current WorkItem is runnable, wake and continue it;
2. otherwise, if another queued runnable WorkItem exists, wake the agent so
   it can explicitly pick one;
3. otherwise, remain idle.

The runtime may surface candidate WorkItems, but it should not silently mutate
`current_work_item_id`.

New ingress should not automatically preempt the current WorkItem. If the
ingress belongs to the same objective, it updates the current WorkItem. If it
forms a different objective, it becomes separate queued work or remains in the
external system that owns routing.

## Persistence Model

`WorkItem` should be persisted as a first-class runtime record.

The persisted record owns:

- objective text;
- lifecycle state;
- plan status;
- durable plan artifact metadata;
- todo-list snapshot;
- blocker text;
- completion summary metadata.

`current_work_item_id` is per-agent focus state. It should not be inferred from
WorkItem lifecycle state.

`AgentState` remains the home for:

- runtime posture;
- wake/sleep state;
- continuation state;
- compacted context metadata;
- other per-agent lifecycle state.

The first implementation may store WorkItem updates append-only, following the
same persistence style as other runtime snapshots.

The plan body should not be duplicated into every WorkItem snapshot. Runtime
snapshots record the descriptor and refreshed preview metadata. The preview is a
cache; the plan file is the source of truth.

## Prompt Projection

At the start of a turn, the runtime should inject a compact work summary.

For the current WorkItem, projection should include:

- `id`
- `objective`
- `state`
- `plan_status`
- `plan_artifact` descriptor with preview and `preview_complete`
- `todo_list`
- `blocked_by` when present

The projection should make priority clear:

1. `objective` says what work this is;
2. the plan artifact says the durable interpretation and approach;
3. `todo_list` says current progress.

The agent should not treat `todo_list` as the task boundary. If `objective`,
the plan artifact, and `todo_list` conflict, the plan is the stronger semantic
anchor.

Other open WorkItems should be summarized compactly by id, objective, state,
plan artifact preview, readiness, current todo, and blocker. Completed
WorkItems should not replay raw transcript by default. They may appear as
bounded recent completed summaries only when they have an explicit promoted
completion report: prefer a non-empty `WorkItemRecord.result_summary`; otherwise
use the newest non-empty `DeliverySummaryRecord.text` for the same work item
(see "Work-Queue Prompt Projection" in `docs/runtime-spec.md`).

If the agent changes focus during a turn, the tool result must return the new
current WorkItem snapshot and state that subsequent tool calls in the turn are
bound to the new current WorkItem unless another id is explicit.

## Tool Model

The tool surface should be action-oriented.

The initial tools are:

- `CreateWorkItem`
- `PickWorkItem`
- `UpdateWorkItem`
- `CompleteWorkItem`
- `GetWorkItem`
- `ListWorkItems`

There is no separate WorkPlan tool in this model. The plan body is a normal
AgentHome file artifact owned by the WorkItem, and `todo_list` is the structured
checklist field.

### CreateWorkItem

`CreateWorkItem` creates a new open WorkItem for a genuinely separate
objective.

Shape:

- `objective` required
- `plan_status` optional
- `todo_list` optional

`CreateWorkItem` should create the WorkItem plan file under AgentHome and return
its artifact descriptor. Agents should create a draft WorkItem when bounded
inspection is needed before the plan is ready, then edit the plan file directly
with normal file tools.

### PickWorkItem

`PickWorkItem` sets `current_work_item_id`.

Shape:

- `work_item_id` required

The tool should return:

- the new current WorkItem snapshot;
- the previous current WorkItem snapshot when present;
- a binding note for the rest of the turn.

### UpdateWorkItem

`UpdateWorkItem` updates mutable fields for an existing WorkItem.

Shape:

- `work_item_id` required
- `objective` optional
- `plan_status` optional
- `todo_list` optional

`objective`
- refines the short target for the same underlying WorkItem.

`plan_status`
- records whether the durable plan is draft, ready, or needs input.

`todo_list`
- replaces the full checklist snapshot.

Waiting state is recorded through `WaitFor`, not direct `UpdateWorkItem`
blocker fields.

Todo item states are:

- `pending`
- `in_progress`
- `completed`

The agent should edit the WorkItem plan file before making a scope or
interpretation change visible in code, then use `UpdateWorkItem` to update
`plan_status`, `todo_list`, or other WorkItem state as needed.

### CompleteWorkItem

`CompleteWorkItem` marks a WorkItem completed.

Shape:

- `work_item_id` required

The target tool contract should not ask the agent to duplicate the completion
report in a tool argument.

When the same assistant round contains both operator-facing completion report
text and a successful `CompleteWorkItem` call for the focused WorkItem, the
runtime should promote that text into the WorkItem result summary, delivery
summary, and completion brief.
The promoted completion brief is the terminal user-facing delivery for that
turn; runtime finalization should not emit a second result brief with the same
completion.

The promoted result summary is not a full progress log. Detailed evidence
remains in transcript, tool records, briefs, verification output, PRs, issues,
and delivery summaries.

### Read Tools

Read shapes:

- `GetWorkItem(work_item_id, include_todo_list?)`
- `ListWorkItems(filter?, limit?, include_todo_list?)`

`include_plan` is deprecated and should be removed from the contract. WorkItem
read tools should always return a plan artifact descriptor and bounded preview,
never the full plan body. The preview must include a `preview_complete` marker
so the agent can tell whether reading the artifact file is necessary.

Useful initial filters are:

- current WorkItem;
- open WorkItems;
- queued open WorkItems;
- blocked open WorkItems;
- completed WorkItems.

Read tools exist so prompt projection does not become a hidden database query
surface. Agents should use them before switching, completing, or materially
changing cross-turn work.

## Todo-List Semantics

Todo-list updates use full-snapshot replacement semantics.

When one todo item changes state, the agent should submit the current full
todo-list snapshot.

This keeps the first version simple:

- the agent rewrites the current list;
- the runtime stores the latest list;
- prompt projection reads one stable snapshot.

Todo items should be operational progress markers, not durable scope records.
The durable scope and approach belong in the WorkItem plan artifact.

## Delegation And Child Agents

Child-agent delegation should not be represented by a generic parent field on
WorkItem.

The ordinary WorkItem model should stay flat:

- `CreateWorkItem` creates one WorkItem for the current agent;
- same-agent decomposition is represented in `todo_list`;
- cross-agent delegation is represented by a structured delegation record.

### SpawnAgent And WorkItem Delegation

`SpawnAgent` does not accept WorkItem delegation metadata.

The spawn surface has one caller-provided text field:

```text
SpawnAgent(
  initial_message,
  preset?,
  agent_id?,
  template?,
  workspace_mode?
)
```

For `private_child`, `initial_message` is required. The runtime delivers it as
the child agent's first delegation message and derives the stable
parent-supervised task label from it at spawn time.

For `public_named`, `initial_message` is optional bootstrap input and does not
create a parent-supervised task.

Spawned agents create or update WorkItems through normal child-side message
handling, for example with `CreateWorkItem`, `PickWorkItem`, and
`UpdateWorkItem`. The parent spawn API must not inject a child WorkItem or set a
child agent's `current_work_item_id`.

### Delegation Record

A delegation record should include:

- `delegation_id`
- `parent_agent_id`
- `parent_work_item_id`
- `child_agent_id`
- `child_work_item_id`
- `state`
- `result_summary` when complete

Delegation state should be separate from WorkItem blocker state.

Spawning a child agent does not automatically make the parent WorkItem blocked.
The parent may continue working, switch to another WorkItem, or call `WaitFor`
with `wake=task_result` if it is truly waiting on the child supervision task.

Child-agent results must be associated back to the parent WorkItem through the
delegation record, not by looking at the parent agent's current focus when the
result is delivered.

## Completion Boundary

Completion belongs to WorkItem.

It should not be overloaded onto:

- turn settlement;
- internal task termination;
- raw ingress exhaustion;
- todo-list exhaustion alone.

`CompleteWorkItem` should require a clear agent action plus minimal runtime
fact checks. The runtime should guard against obvious contradictions, such as:

- picking or completing a WorkItem that does not belong to the agent;
- picking a WorkItem that is already completed;
- completing a WorkItem while clearly unfinished blocking execution remains;
- setting an empty blocker.

The runtime should not attempt full semantic judgment about whether the
objective is truly satisfied.

## Default Bias

The initial default bias should be conservative:

- if the agent does not pick a different WorkItem, keep the current WorkItem;
- if the plan is not ready and the task is nontrivial, stay in planning or mark
  `needs_input`;
- if the agent needs to change interpretation, edit the plan artifact before
  patching;
- blocked state should be explicit through `blocked_by`;
- completion should require explicit completion action.

## Current Design Direction

This RFC currently assumes:

- WorkItem boundaries are determined by `objective`, interpreted through the
  plan artifact;
- `objective` replaces the old delivery-target framing;
- the plan body is a durable AgentHome file artifact;
- `todo_list` is only the structured progress checklist;
- todo item states are `pending`, `in_progress`, and `completed`;
- item-level blocked state is omitted;
- WorkItem-level blocked state is represented by `blocked_by`;
- the first rollout remains message-driven by default;
- WorkItem creation is explicit rather than inferred from every ingress;
- `current_work_item_id` is controlled by explicit agent action;
- queued and blocked views are derived from `open`, `current_work_item_id`, and
  `blocked_by`;
- runtime does not silently switch current work;
- progress narration remains in transcript, brief, tool, issue, PR, and final
  message records associated back to WorkItems by runtime binding;
- child-agent delegation starts with `SpawnAgent(initial_message=...)`, while
  child WorkItems are created or updated through normal child-side tool calls
  and delegation records, not `WorkItem.parent_id`;
- external systems may own queues, but agent-facing WorkItem is the durable
  current-work anchor.
