---
title: RFC: WorkItem Current Focus Canonical Fact
date: 2026-07-16
status: accepted
handle: rfc-work-item-current-focus
---

# RFC: WorkItem Current Focus Canonical Fact

## Summary

Durable WorkItem focus models the single-valued relation
`agent -> open WorkItem?`. In legacy and scheduler-shadow modes,
`agent_states.current_work_item_id` is the production-authoritative storage
fact. The normalized scheduler protocol imports the same relation into
`scheduler_agent_focus`; after guarded authoritative cutover, that row is the
canonical protocol fact and `agent_states.current_work_item_id` is an
atomically dual-written compatibility projection.

`work_items.current_focus` is a deprecated compatibility column. Runtime code
must neither read nor write it. WorkItem views derive currentness by joining
the authority selected for the active scenario mode to the owned open
WorkItem. The two supported focus locations must agree before cutover or
rollback; disagreement is a hard blocker, never a precedence rule.

`AgentState.current_turn_work_item_id` is turn attribution, not a fallback
focus fact. It may retain the WorkItem that owns an already admitted turn even
after durable focus changes.

## Invariants

For every non-null canonical focus:

1. the target WorkItem exists;
2. the target belongs to the same agent;
3. the target is open;
4. one agent has at most one focus because both focus tables have one row per
   `agent_id`; and
5. guarded dual-write commits the same target and revision transition to the
   authoritative row and compatibility projection.

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

After preflight, every legacy `work_items.current_focus` value is cleared. The
column may be physically removed in a later table-rebuild migration.

Phase 2 then imports one explicit `scheduler_agent_focus` row per agent,
including a null target and focus revision when no WorkItem is focused.
Import, dual-write comparison, authoritative cutover, and rollback follow the
scenario gates in
[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md).
An absent normalized row or a disagreement with the current agent-state fact
blocks cutover.

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
  releases the current Turn binding and execution ownership;
- WorkItem-scoped waiting does not clear canonical durable focus unless the
  same settlement contains an explicit focus transition; and
- scheduler admission uses activation binding and lane state rather than
  treating durable focus as execution ownership.

Automatic rehydration requires an ingress binding issued by the runtime, such
as a `wait_id` or equivalent continuation token. Current ordinary
`OperatorPrompt` ingress has no such binding, so the runtime must not guess by
wait count, recency, text, or transport identity. Without a verified binding,
the agent or operator explicitly picks the waiting WorkItem.

The target activation protocol is specified by
[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md).

## Restart And Projections

Restart restores focus only from the authority selected for the active
scenario mode: the agent-state row in legacy or shadow mode, and
`scheduler_agent_focus` in authoritative mode. It never falls back from a
missing or invalid authoritative row to the compatibility projection.
In-memory `AgentState`, prompt projections, scheduler views, HTTP responses,
and TUI state are rebuildable projections. Post-commit projection failures do
not change the committed transition result.
