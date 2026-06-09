---
title: RFC: Work Item Continuation Stack
date: 2026-06-09
status: draft
issue:
  - 1617
---

# RFC: Work Item Continuation Stack

## Summary

Holon should let an open WorkItem yield execution to another WorkItem without
marking the yielding WorkItem as blocked.

The missing state is a runtime-owned continuation frame:

```text
A yielded to B
resume A when B reaches the configured return condition
```

This is different from `WaitFor`. A wait says useful progress is blocked by a
future condition. A continuation frame says the agent intentionally moved the
active control flow from one WorkItem to another and expects to return.

## Problem

Track or orchestrator WorkItems often create or select concrete WorkItems.

Example:

```text
T = track umbrella WorkItem
A = concrete WorkItem for one issue or PR
```

The agent starts in `T`, switches to `A`, and waits for `A`'s CI/review/merge
state. While `A` is active, `T` should not keep getting scheduled just because it
is open. But `T` also should not be represented as blocked by CI, because the CI
belongs to `A`.

The current model leaves two poor choices:

- keep `T` runnable, which can cause repeated work-queue ticks and prompt
  pressure to re-enter `T`;
- mark `T` blocked, which suppresses repeated scheduling but loses the natural
  return path when `A` completes.

This is not primarily a parent/child modeling problem. It is a control-flow
problem: the runtime needs to remember that `T` yielded to `A`.

## Goals

- Represent "temporarily not schedulable because execution yielded to another
  WorkItem" as a first-class runtime fact.
- Keep yielded WorkItems out of queued runnable scheduling.
- Resume the yielded WorkItem when the active WorkItem reaches the configured
  return condition.
- Keep WorkItem waits scoped to the WorkItem that actually owns the external,
  task, timer, operator, or system condition.
- Preserve the invariant that queued WorkItems do not silently become current
  without an explicit or runtime-explained transition.

## Non-Goals

- Do not make WorkItems a strict parent/child tree.
- Do not replace `WaitFor`.
- Do not make every relation between WorkItems a scheduling dependency.
- Do not require a general graph scheduler before the WorkItem control-flow
  problem is solved.
- Do not make blocked state a catch-all for "not currently selected".

## Terms

### Runnable

An open WorkItem that is eligible for work-queue scheduling.

### Blocked

An open WorkItem whose own progress is waiting on a blocking condition such as
task result, external change, operator input, timer, or an explicit blocker.

### Parked / Yielded

An open WorkItem that is not runnable because it yielded control to another
WorkItem. It is not blocked. The runtime knows what WorkItem currently owns the
continuation and when to resume the yielded item.

### Continuation Frame

A runtime record that captures a temporary WorkItem control-flow transfer.

```text
WorkItemContinuationFrame {
  id
  agent_id
  suspended_work_item_id
  active_work_item_id
  return_policy
  state
  created_at
  updated_at
  resolved_at?
  cancelled_at?
  reason?
}
```

Initial return policies:

- `on_completed`: resume the suspended WorkItem when the active WorkItem is
  completed.

Phase one implements only `on_completed`. Other return policies can be added
when there is a concrete runtime need.

Initial states:

- `active`
- `resumed`
- `cancelled`

## Proposed Model

### Scheduling State

Extend WorkItem scheduling posture with a yielded state:

```text
Open WorkItem scheduling posture:
- current/runnable
- queued/runnable
- yielded_to_work_item
- waiting_task
- waiting_external
- waiting_operator
- waiting_timer
- waiting_system
- blocked/manual
- completed
```

`yielded_to_work_item` is derived from an active continuation frame where the
WorkItem is `suspended_work_item_id`.

The yielded WorkItem:

- remains `state = open`;
- does not need `blocked_by`;
- does not appear in queued runnable candidates;
- may appear in prompt/list projections as parked/yielded;
- becomes runnable again when its continuation frame is resumed or cancelled.

### Tool Surface

Phase one should improve `PickWorkItem` rather than introduce a separate
continuation tool.

When the agent has an open runnable current WorkItem `A` and calls
`PickWorkItem(B)` for a different open WorkItem, phase one defaults to yielding
the current item:

```text
current = A
PickWorkItem(B)
=> create WorkItemContinuationFrame(A yielded_to B, return_policy=on_completed)
=> make B current
```

This matches the agent's natural control-flow action: "switch to B, then come
back to A when B is done."

Defaulting rules:

- if there is a different open runnable current WorkItem, create an active
  `on_completed` frame from current to target;
- if there is no current WorkItem, the target is the current WorkItem, the
  current WorkItem is not runnable, or the target is not open, do not create a
  frame and keep existing pick validation semantics;
- if the target WorkItem is already parked/yielded, explicit pick resumes the
  matching frame with reason `explicit_pick` so the target cannot remain
  yielded after becoming current.

Phase one intentionally does not add public `mode`, `return_to`, or `replace`
parameters. The runtime infers the return target from current focus. The stored
fact is still an explicit continuation frame rather than a synthetic blocker.

### CompleteWorkItem

When `CompleteWorkItem(B)` succeeds:

1. Complete `B` using the existing completion rules.
2. Cancel or resolve active wait conditions owned by `B` as today.
3. Find the active direct-caller continuation frame with
   `active_work_item_id = B`.
4. If its return policy is satisfied:
   - mark the frame `resumed`;
   - set `current_work_item_id` to `suspended_work_item_id`;
   - set the current turn binding to `suspended_work_item_id` when continuing
     inside the same runtime turn would otherwise require another model pick;
   - emit visible audit/scheduler evidence with reason
     `continuation_resumed`.

This is an explicit stack return, not a queued WorkItem silently replacing
current focus. The continuation frame is the evidence that authorizes the focus
restore.

After completing `B` and restoring `A`, the current model turn should close as
continuable. The scheduler should then start the next pass anchored on the
resumed WorkItem. The agent should not need to call `PickWorkItem(A)` merely to
restore the stack.

### Explicit Switching

If the agent explicitly picks a parked WorkItem before the frame is resumed, the
runtime should either:

- cancel the matching continuation frame with reason `explicit_pick`; or
- mark it resumed if the explicit pick is treated as the return.

The important invariant is that a parked WorkItem cannot remain parked after the
agent has explicitly made it current.

### Relation Boundary

WorkItem relations describe durable work structure:

```text
T tracks A
T depends_on A
T spawned A
```

Relations are not enough to define scheduling. A tracking relation does not
mean `T` is blocked by `A`, and it does not by itself define whether `T` should
yield to `A`.

The control-flow handoff is represented by a continuation frame:

```text
T yielded to A until A completed
```

This keeps three concepts separate:

- relation: long-lived structure between WorkItems;
- continuation frame: temporary control-flow transfer;
- wait condition: blocking future condition owned by a WorkItem.

### WaitFor Boundary

`WaitFor` remains the right tool when the WorkItem itself cannot continue until
a condition is satisfied:

```text
A waits for github_ci:123
A waits for operator input
A waits for task result
```

`WaitFor(resource = work_item:B)` may still be useful for true dependencies:

```text
Release waits for PackageBuild to complete
```

But a track WorkItem yielding to a concrete WorkItem is not necessarily a wait.
It is a continuation transfer. Treating it as a wait works mechanically, but it
overloads waiting language and makes the WorkItem look blocked when it is only
parked.

## Prompt And Query Projection

Prompt projection should expose yielded WorkItems compactly, separate from
blocked WorkItems:

```text
Parked work items:
- [yielded_to_work_item] T :: tracking issue set :: yielded_to=A return=on_completed
```

`ListWorkItems` supports a yielded readiness value and a `yielded` filter.
`GetWorkItem(T)` reports yielded readiness/focus when `T` is suspended by an
active frame.

Completion tool results should consider returning a compact reactivation hint:

```text
continuation_resumed:
  suspended_work_item_id: T
  reason: continuation_resumed
  active_work_item_id: A
```

This is optional. The preferred phase-one behavior is for
`CompleteWorkItem(active)` to finish the current turn after restoring the caller
and let the scheduler resume execution from the restored WorkItem. A compact
reactivation hint can still be useful for logs or clients, but it is not the
primary agent control path.

## Scheduler Contract

The scheduler should treat yielded WorkItems as non-runnable until their
continuation frame is resolved or cancelled.

Priority order becomes:

1. current runnable WorkItem;
2. current WorkItem restored by a resumed continuation frame;
3. triggered blocked WorkItems as review candidates;
4. queued runnable WorkItems;
5. yielded/parked WorkItems as compact context only;
6. blocked WorkItems as compact context or recheck candidates;
7. idle or sleep.

Yielded WorkItems should not emit `queued_available` ticks while their
continuation frame remains active.

When a frame resumes, the scheduler should use a distinct reason such as:

```text
work_queue:continuation_resumed:<suspended_work_item_id>:<frame_id>:<revision>
```

This keeps the wake explainable and avoids conflating it with ordinary queued
work.

Because stack resume restores `current_work_item_id`, this tick is not asking
the model to choose among queued candidates. It is a continuation pass for the
resumed caller.

## Storage And Ledger

The persisted record should be separate from `WorkItemRecord`.

Table/log:

```text
work_item_continuations
```

Fields:

- `id`
- `agent_id`
- `suspended_work_item_id`
- `active_work_item_id`
- `return_policy`
- `state`
- `reason`
- `created_at`
- `updated_at`
- `resolved_at`
- `cancelled_at`
- `resolution_reason`

Indexes should support:

- active frames by suspended WorkItem;
- active frames by active WorkItem;
- latest frames by agent;
- reactivation lookup during `CompleteWorkItem`.

The runtime should emit audit events for:

- frame created;
- frame resumed;
- frame cancelled;
- yielded WorkItem reactivated.

## Invariants

- A completed WorkItem cannot be active or suspended in a new active frame.
- A WorkItem with an active frame as `suspended_work_item_id` is not queued
  runnable.
- A WorkItem made current by explicit pick cannot remain suspended by an active
  frame.
- Phase one active frames form a stack: each suspended WorkItem has at most one
  active callee, each active WorkItem has at most one direct caller, and active
  frames must be acyclic.
- A WorkItem's own wait conditions still belong to that WorkItem, even when it
  is the active target of another WorkItem's continuation frame.
- Completion of the active WorkItem resumes only frames whose return policy is
  satisfied.
- Frame resumption should be idempotent.

## Example Flow

```text
T = Track issue set
A = Fix issue 1617

1. Agent is current on T.
2. Agent creates or selects A.
3. Agent calls `PickWorkItem(A)`.
4. Runtime records:
   WorkItemContinuationFrame(T yielded_to A, return_policy=on_completed)
5. Runtime makes A current.
6. A calls WaitFor(resource=github_ci, wake=external).
7. T is projected as yielded, not runnable and not blocked.
8. CI wakes A.
9. Agent completes A.
10. Runtime resumes the frame and restores T as current.
11. The turn closes as continuable.
12. Scheduler starts the next pass anchored on T.
13. T updates progress or yields to the next item.
```

## Implementation Phases

### Phase 1: RFC And Projection Terms

- Add the RFC.
- Add terminology to WorkItem scheduler and prompt projection docs.

### Phase 2: Runtime Record

- Add `WorkItemContinuationFrame` type and storage.
- Derive yielded scheduling state from active frames.
- Add query/projection support.

### Phase 3: Tool Surface

- Default from a different open runnable current WorkItem to runtime-owned
  yield.
- Do not add public `mode`, `return_to`, or `replace` arguments in phase one.
- Validate agent ownership and open WorkItem state.
- Cancel or resume frames on explicit picks.

### Phase 4: Reactivation

- Resume frames from `CompleteWorkItem`.
- Restore `current_work_item_id` to the suspended caller.
- Close the completion turn as continuable.
- Emit continuation-specific scheduler/audit events.
- Optionally include `continuation_resumed` in completion tool output for
  clients and logs, not as the primary agent control path.

### Phase 5: Relation Integration

- If WorkItem relations are added, allow tools to create relation and
  continuation records together, while keeping relation semantics distinct from
  scheduling.

## Open Questions

- When `CompleteWorkItem(B)` restores `A`, should the runtime always close the
  current turn immediately, or only when the completion consumed the active
  stack frame?
- Should nested stack depth be capped in phase one to prevent accidental long
  chains?
- Should clients receive `continuation_resumed` in tool results even though
  scheduler-driven resume is the primary control path?

## Decision

Adopt the concept of WorkItem continuation frames as the canonical way to model
"A yielded to B; resume A when B is done."

Phase one should use stack semantics:

- `PickWorkItem(B)` from an open runnable current WorkItem `A` yields `A` to
  `B` by default.
- No public `PickWorkItem` mode is added in phase one.
- `CompleteWorkItem(B)` resumes the direct caller `A`, restores current focus to
  `A`, and lets the scheduler continue from that restored frame.
- Active frames form an acyclic single-caller/single-callee chain, not a general
  dependency graph.

Do not encode this state as `blocked_by`.

Do not rely only on WorkItem relation metadata.

Keep waits, relations, and continuation frames as separate runtime facts.
