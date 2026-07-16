---
title: RFC: WorkItem Current Focus Canonical Fact
date: 2026-07-16
status: accepted
handle: rfc-work-item-current-focus
---

# RFC: WorkItem Current Focus Canonical Fact

## Summary

`agent_states.current_work_item_id` is the only canonical durable WorkItem
focus fact. It models the single-valued relation `agent -> open WorkItem?`.

`work_items.current_focus` is a deprecated compatibility column. Runtime code
must neither read nor write it. WorkItem views derive currentness by joining
the agent state pointer to the owned open WorkItem.

`AgentState.current_turn_work_item_id` is turn attribution, not a fallback
focus fact. It may retain the WorkItem that owns an already admitted turn even
after durable focus changes.

## Invariants

For every non-null canonical focus:

1. the target WorkItem exists;
2. the target belongs to the same agent;
3. the target is open;
4. one agent has at most one focus because `agent_states` has one row per
   `agent_id`.

SQLite triggers enforce target existence, ownership, and open state. Runtime
transition commands validate the same rules to return useful errors before
the trigger is the last line of defense.

## Migration

Migration preflight compares `agent_states.current_work_item_id` with legacy
`work_items.current_focus` rows:

- matching valid facts migrate unchanged;
- a valid agent-state pointer with no legacy focused row remains canonical;
- a null agent-state pointer with exactly one valid legacy focused row is
  backfilled into both the `agent_states` column and JSON payload;
- conflicting pointers, multiple legacy focused rows, or missing, foreign, or
  completed targets fail migration with identifying diagnostics.

After preflight, every legacy `current_focus` value is cleared. The column may
be physically removed in a later table-rebuild migration.

## Atomic Transition Write Sets

Focus transitions use one restricted runtime transition transaction.

| Transition | Atomic durable facts |
| --- | --- |
| pick | optional blocker/wait cancellation, continuation create/resolve/cancel, agent focus and turn binding, audits, index outbox |
| complete without return | completed WorkItem, owned wait cancellation, focus/turn release when matched, audits, index outbox |
| complete with return | completed WorkItem, owned wait cancellation, continuation resolution, restored caller focus and turn binding, audits, index outbox |
| explicit yielded return | continuation resolution, restored focus and turn binding, audits |
| invalid return target | continuation cancellation and deterministic focus fallback in the same transaction |

No committed state may expose a continuation-frame change without its matching
focus change, or a completed focused WorkItem.

## Operator Input And Sticky Association

Focus, plan status, waits, and turn attribution remain orthogonal:

- ordinary turn completion does not release focus;
- setting `plan_status=needs_input` alone does not release focus;
- WorkItem-scoped `WaitFor(operator_input)` atomically records the wait and
  releases execution focus;
- a read model may still present that wait-scoped WorkItem as a sticky
  attention target, but it is not current execution focus.

Automatic rehydration requires an ingress binding issued by the runtime, such
as a `wait_id` or equivalent continuation token. Current ordinary
`OperatorPrompt` ingress has no such binding, so the runtime must not guess by
wait count, recency, text, or transport identity. Without a verified binding,
the agent or operator explicitly picks the waiting WorkItem.

## Restart And Projections

Restart restores focus only from the canonical agent-state row. In-memory
`AgentState`, prompt projections, scheduler views, HTTP responses, and TUI
state are rebuildable projections. Post-commit projection failures do not
change the committed transition result.
