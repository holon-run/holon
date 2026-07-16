---
title: RFC: WorkItem Scheduling Read Model
date: 2026-07-16
status: accepted
handle: rfc-work-item-scheduling-read-model
---

# RFC: WorkItem Scheduling Read Model

## Summary

WorkItem lifecycle, plan status, manual blockers, active `WaitCondition`s,
continuation frames, and canonical current focus are independent durable facts.
They are projected through one shared scheduling read model. Scheduler,
closure, prompt/context, WorkItem tools, HTTP, and TUI must consume that
projection rather than reproducing precedence rules.

`readiness`, scheduling state, candidate class, focus view, posture, and
closure outcome are derived values. They are not persisted as authoritative
WorkItem state. The legacy `work_items.readiness` column is removed.

## Canonical Facts

The read model may use only:

- `WorkItemRecord.state`;
- `WorkItemRecord.plan_status`;
- `WorkItemRecord.blocked_by` and recheck metadata;
- active WorkItem-scoped `WaitConditionRecord`s;
- active continuation frames in which the WorkItem is suspended;
- `agent_states.current_work_item_id`;
- external-trigger delivery evidence associated with active waits; and
- todo/timestamps for display and ordering only.

`AgentState.current_turn_work_item_id` is turn attribution and is never a
fallback current-focus fact.

A blocking `TaskRecord` is execution metadata, not a WorkItem waiting fact.
Only an active task `WaitCondition` projects `WaitingTask`.

## Precedence

The shared projection applies this order:

1. `Completed`
2. `YieldedToWorkItem`
3. active `WaitCondition`
4. manual `Blocked`
5. `plan_status=needs_input` as `WaitingOperator`
6. `Runnable`

When legacy or corrupt data contains multiple active wait kinds, projection is
deterministic:

`Task > Operator > Timer > External > System`.

All active waits remain visible and the projection emits a diagnostic.

Focus is a separate facet. Currentness comes only from canonical agent focus.
A completed WorkItem cannot be current. A suspended continuation normally
cannot be current; suspicious combinations are reported without mutating
durable state.

## Golden Matrix

| Lifecycle | Yielded | Active wait | Blocker | Plan | Scheduling state | Readiness | Reason |
| --- | --- | --- | --- | --- | --- | --- | --- |
| completed | any | any | any | any | completed | completed | completed |
| open | yes | any | any | any | yielded_to_work_item | yielded | continuation_yielded |
| open | no | task | any | needs_input | waiting_task | blocked | active_task_wait |
| open | no | operator | any | ready | waiting_operator | waiting_for_operator | active_operator_wait |
| open | no | timer | any | any | waiting_timer | blocked | active_timer_wait |
| open | no | external | any | any | waiting_external | blocked | active_external_wait |
| open | no | system | any | any | waiting_system | blocked | active_system_wait |
| open | no | none | yes | needs_input | blocked | blocked | manual_blocker |
| open | no | none | no | needs_input | waiting_operator | waiting_for_operator | plan_needs_input |
| open | no | none | no | draft/ready | runnable | runnable | runnable |

The matrix fixes two previously divergent cases:

- an explicit active wait outranks `needs_input`;
- a task wait exists only when a task `WaitCondition` exists.

## Candidate Classes And Ordering

Candidate classes are derived from the same item projection:

1. current runnable;
2. triggered blocked;
3. queued runnable;
4. yielded;
5. waiting for operator;
6. blocked;
7. recently completed.

Ordering is stable within each class and is shared by prompt, HTTP, and TUI.
Surfaces may truncate or group the projection but may not reinterpret it.

## Public Projection

HTTP state and WorkItem query endpoints return the shared projection. Canonical
record fields remain available for compatibility, while the response adds:

- `scheduling_state`;
- `readiness`;
- `candidate_class`;
- `focus`;
- `is_current`;
- active-wait facets and summaries;
- `reason_code`; and
- projection diagnostics.

TUI consumes this projection directly. Stream events that change WorkItem,
wait, continuation, or focus facts invalidate the WorkItem slice and trigger a
fresh snapshot; TUI does not reconstruct scheduling state locally.

## Sticky Operator Input

The current-focus contract from `work-item-current-focus.md` remains:

- `plan_status=needs_input` may keep canonical focus;
- WorkItem-scoped `WaitFor(operator_input)` records an operator wait and
  releases execution focus;
- an unbound ordinary `OperatorPrompt` does not guess which waiting WorkItem to
  rehydrate.

The read model presents the former as a current `WaitingOperator` projection
and the latter as a non-current waiting attention target.

## Invariants And Diagnostics

The projection is total: suspicious combinations do not panic and do not
silently change durable facts. Diagnostics cover at least:

- completed WorkItem marked current;
- yielded WorkItem marked current;
- multiple active wait kinds; and
- active waits attached to a completed WorkItem.

Core properties:

- completed is never runnable or current in the public projection;
- yielded is never queued runnable;
- active wait reason is retained;
- a triggered wait does not clear a manual blocker;
- plan status never overrides lifecycle or an active wait; and
- record-only and fully loaded projections use the same derivation function.

## Migration

1. Introduce the pure scheduling projection and queue read model.
2. Make storage load canonical facts and call the pure projection.
3. Migrate scheduler, closure, prompt/context, tools, HTTP, and TUI.
4. Stop writing `work_items.readiness`.
5. Drop `idx_work_items_readiness` and the `readiness` column.
6. Delete local readiness/focus/candidate precedence implementations.

