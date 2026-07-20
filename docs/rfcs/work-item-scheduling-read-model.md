---
title: RFC: WorkItem Scheduling Read Model
date: 2026-07-16
status: accepted
handle: rfc-work-item-scheduling-read-model
---

# RFC: WorkItem Scheduling Read Model

## Summary

WorkItem lifecycle, dispatch generation, manual holds, active and triggered
`WaitCondition`s, activation and settlement state, continuation frames, and
canonical current focus are independent durable facts. They are projected
through one shared scheduling read model. Scheduler, closure, prompt/context,
WorkItem tools, HTTP, and TUI must consume that projection rather than
reproducing precedence rules.

`readiness`, scheduling state, candidate class, focus view, posture, and
closure outcome are derived values. They are not persisted as authoritative
WorkItem state. The legacy `work_items.readiness` column is removed.

## Canonical Facts

The read model may use only:

- `WorkItemRecord.state`;
- scheduler-sensitive dispatch generation or its canonical source facts;
- typed manual hold and compatibility blocker/recheck facts during migration;
- WorkItem-scoped `WaitConditionRecord`s across `Active`, `Triggered`,
  `Consumed`, and terminal states, including owner, wait generation, trigger
  generation, and consuming activation identity;
- `AgentActivation` state and binding, plus matching settlement identity or
  durable settlement-missing evidence;
- active continuation frames in which the WorkItem is suspended;
- `agent_states.current_work_item_id`;
- external-trigger delivery evidence associated with active waits; and
- todo/timestamps for display and ordering only.

`AgentState.current_turn_work_item_id` is turn attribution and is never a
fallback current-focus fact.

`plan_status`, todo contents, plan text, assistant prose, and brief contents
are coordination or display facts. They do not create or suppress scheduler
demand.

A blocking `TaskRecord` is execution metadata, not a WorkItem waiting fact.
Only an active task `WaitCondition` projects `WaitingTask`.

An `Active` wait and a `Triggered` wait whose trigger generation has not been
consumed are blocking wait facts. A `Consumed` wait is not a waiting fact; it
must correlate with its consuming activation and remains available for
settlement, rearm, recovery, and divergence diagnostics. Rearm is observed as
`Resolved(g) + Active(g+1)`, never by treating `Consumed(g)` as active again.

`NeedsSettlement` is derived from canonical
`ActivationState::SettlementMissing` and its WorkItem binding. A missing join
or absent optional projection field is not enough to invent this state.

## Precedence

The shared projection applies this order:

1. `Completed`
2. `NeedsSettlement`
3. `Executing`
4. `YieldedToWorkItem`
5. blocking `WaitCondition` (`Active` or triggered-unconsumed)
6. manual `Paused`
7. offered scheduling generation as `Runnable`

When legacy or corrupt data contains multiple active wait kinds, projection is
deterministic:

`Task > Operator > Timer > External > System`.

All blocking waits remain visible and the projection emits a diagnostic.

Focus is a separate facet. Currentness comes only from canonical agent focus.
A completed WorkItem cannot be current. A suspended continuation normally
cannot be current; suspicious combinations are reported without mutating
durable state.

## Golden Matrix

| Lifecycle | Activation | Yielded | Blocking wait | Hold | Offered generation | Scheduling state | Readiness | Reason |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| completed | any | any | any | any | any | completed | completed | completed |
| open | settlement missing | any | any | any | any | needs_settlement | blocked | settlement_missing |
| open | running | any | any | any | any | executing | blocked | activation_running |
| open | none | yes | any | any | any | yielded_to_work_item | yielded | continuation_yielded |
| open | none | no | active task | any | any | waiting_task | blocked | active_task_wait |
| open | none | no | active operator | any | any | waiting_operator | waiting_for_operator | active_operator_wait |
| open | none | no | active timer | any | any | waiting_timer | blocked | active_timer_wait |
| open | none | no | active external | any | any | waiting_external | blocked | active_external_wait |
| open | none | no | triggered-unconsumed | any | any | waiting_<kind> | blocked | wait_triggered_unconsumed |
| open | none | no | none | yes | any | paused | blocked | manual_hold |
| open | none | no | none | no | yes | runnable | runnable | dispatch_offered |
| open | none | no | none | no | no | idle | blocked | no_dispatch_offer |

The matrix fixes previously divergent cases:

- an explicit active wait is the waiting authority, not `plan_status`;
- a task wait exists only when a task `WaitCondition` exists;
- metadata edits do not create a new runnable generation; and
- a running or settlement-missing activation outranks queued demand.

Consumed and terminal waits do not occupy the blocking-wait column. They
remain projection facets and diagnostics. An orphan consumed wait emits a
diagnostic and must be repaired through activation/settlement recovery; the
read model does not reinterpret it as active or synthesize runnable demand.

## Candidate Classes And Ordering

Candidate classes are derived from the same item projection:

1. current runnable;
2. triggered blocked (an exact triggered-unconsumed wait generation);
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
- triggered, consumed, and terminal wait-generation diagnostics;
- running and settlement-missing activation facets;
- `reason_code`; and
- projection diagnostics.

TUI consumes this projection directly. Stream events that change WorkItem,
wait, continuation, or focus facts invalidate the WorkItem slice and trigger a
fresh snapshot; TUI does not reconstruct scheduling state locally.

## Sticky Operator Input

The current-focus contract from `work-item-current-focus.md` remains:

- `plan_status=needs_input` may keep canonical focus but does not create a
  scheduler wait;
- WorkItem-scoped `WaitFor(operator_input)` records an operator wait and
  releases Turn execution ownership;
- the wait does not clear durable focus unless settlement explicitly changes
  it; and
- an unbound ordinary `OperatorPrompt` does not guess which waiting WorkItem to
  rehydrate.

The read model presents waiting only from the active wait. Focus remains a
separate facet.

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
- a triggered wait does not clear a manual hold;
- plan status never changes scheduler state; and
- record-only and fully loaded projections use the same derivation function.

## Migration

1. Introduce the pure scheduling projection and queue read model.
2. Make storage load canonical facts and call the pure projection.
3. Migrate scheduler, closure, prompt/context, tools, HTTP, and TUI.
4. Stop writing `work_items.readiness`.
5. Drop `idx_work_items_readiness` and the `readiness` column.
6. Introduce activation, scheduling-generation, and typed-hold facts under the
   activation protocol feature gate.
7. Stop deriving waiting from `plan_status` and generic blocker text.
8. Delete local readiness/focus/candidate precedence implementations.

The target activation and demand protocol is specified by
[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md).
That RFC's typed `protocol_mode`, per-scenario authority mode, rollout
manifest, and preflight contract define this feature gate; implementations
must not replace them with an unchecked boolean.

