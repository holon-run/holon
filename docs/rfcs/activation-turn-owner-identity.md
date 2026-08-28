---
title: RFC: Activation and Turn Owner Identity
date: 2026-08-28
status: accepted
---

# Activation and Turn Owner Identity

Phase 1 establishes durable owner identity before Holon changes semantic context
projection. It follows
[Deterministic Projection Evaluation Phase 0](./projection-evaluation-phase-0.md).

## Scope

Every canonical execution attempt already has one `ExecutionBinding`. Phase 1
uses that binding as the authority and materializes the same owner on its Turn:

- `WorkItem { work_item_id }`
- `Conversation { interaction_id }`
- `AgentLifecycle { agent_id }`
- `Command`

`TurnRecord.owner` is a query and retention relation. It is not a second
scheduler authority. `current_work_item_id` remains a compatibility projection
and must agree with a WorkItem owner.

This phase does not replace prompt sections, select historical Turns by owner,
or change provider request content. Those are Phase 2 concerns.

## Admission

Owner selection follows trusted admission facts:

1. an exact WorkItem binding always selects `WorkItem`
2. a trusted operator prompt without a WorkItem or exact wait selects
   `Conversation`
3. external, timer, recovery, task, internal lifecycle, and exact wait
   activations remain `AgentLifecycle` unless exactly bound to a WorkItem
4. `Command` remains independent

Conversation identity is opaque and derived from authenticated server-side
facts. Local CLI, run-once, and authenticated control prompts use a stable local
interaction scope. Remote operator transport uses the validated transport
binding plus its optional `conversation_ref`. Arbitrary message metadata cannot
choose an interaction.

Conversation expresses continuity only. Its interaction id owns no WorkItem,
task, wait, completion, or scheduling authority. An agent execution with a
Conversation owner may still use the existing agent-scoped WorkItem tools, but
the target must pass the same agent, source-revision, state, and report-binding
fences; matching an interaction id never satisfies or weakens those fences.

## Persistence And Compatibility

New Turns persist the canonical owner and indexed `(owner_kind, owner_id)`.
Restart reconstruction reads the durable owner rather than process-local
conversation state.

Legacy Turns have no owner field. Their conservative fallback is:

- `current_work_item_id` present: `WorkItem`
- otherwise: `AgentLifecycle`

Legacy unbound facts are never inferred to be Conversation because doing so
could join unrelated history or elevate authority.

An existing legacy Turn may be enriched once with a matching canonical owner.
An existing explicit owner is immutable.

## Authority Invariants

- admission binding, current execution owner, and new Turn owner agree
- Conversation cannot resume a wait or task solely because interaction ids
  match
- exact wait and task-result admission retain their existing authority fences
- untrusted or external metadata cannot create or merge Conversation identity
- WorkItem owner semantics and settlement remain unchanged
- owner reconstruction is deterministic across restart

## Evaluation

`ProjectionManifest.activation_owner` may report the new durable owner, but
Phase 1 must leave rendered system/context sections and provider lowering
unchanged. Candidate manifests are compared with the frozen Phase 0 baseline;
owner-label changes are expected only where admission now records Conversation.

Phase 2 requires separate authorization and must prove owner-thread selection,
single representation per Turn, direct-predecessor retention, and provider
behavior before retiring legacy prompt sections.
