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

Every model Turn is owned by its agent. Its `ExecutionBinding::Agent` carries
the agent ID and a `TurnAttribution` with optional WorkItem and interaction IDs.
Either, both, or neither ID may be present. `ExecutionBinding::Command` remains
an independent non-model binding.

`TurnRecord.owner` retains the query and retention projection: WorkItem takes
precedence over Conversation, otherwise the owner is AgentLifecycle (or
Command for command turns). The Turn stores the full model admission
attribution separately; older records recover only the attribution represented
by their historical owner. Attribution is identity metadata and does not affect
content deletion. The owner projection and
`AgentState.current_execution_binding` are not separate scheduler authorities.
`current_work_item_id` must still agree with a projected WorkItem owner.

This phase does not replace prompt sections, select historical Turns by owner,
or change provider request content. Those are Phase 2 concerns.

## Admission

Owner selection follows trusted admission facts:

1. an exact WorkItem binding always selects `WorkItem`
2. a trusted operator prompt without a WorkItem or exact wait selects
   `Conversation`
3. external, timer, task, internal lifecycle, and exact wait
   activations remain `AgentLifecycle` unless exactly bound to a WorkItem
4. `Command` remains independent

Typed provider recovery is a source-bound exception, not an ordinary lifecycle
wake. After validating runtime provenance, the same-agent source message,
provider-failure terminal Turn, and terminal predecessor attempt, admission
inherits that attempt's entire `ExecutionBinding`, including both attribution
fields. A missing WorkItem or interaction stays missing.
Recovery cannot choose an owner from current focus or message metadata.
Conflicting queued WorkItem bindings fail closed; absence of a source WorkItem
is not permission to adopt the focused one.

Conversation identity is opaque and derived from authenticated server-side
facts. Local CLI, run-once, and authenticated control prompts use a stable local
interaction scope. Remote operator transport uses the validated transport
binding plus its optional `conversation_ref`. Arbitrary message metadata cannot
choose an interaction.

An interaction ID expresses continuity only. It owns no WorkItem, task, wait,
completion, or scheduling authority. Any model Turn may use the existing
agent-scoped WorkItem tools. The target must pass the same agent,
source-revision, state, and report-binding fences; matching an interaction ID
never satisfies or weakens those fences.

## Persistence And Compatibility

New model Turns persist attribution and retain their indexed
`(owner_kind, owner_id)` projection. Restart reconstructs from durable facts,
not process-local conversation state. Historical WorkItem and Conversation
bindings acquire their agent ID from the owning durable attempt or event
envelope, never from ingress metadata. A historical AgentLifecycle binding must
agree with that owner.

Legacy Turns have no owner field. Their conservative fallback is:

- `current_work_item_id` present: `WorkItem`
- otherwise: `AgentLifecycle`

Legacy unbound facts are never inferred to be Conversation because doing so
could join unrelated history or elevate authority.

An existing legacy Turn may be enriched once with a matching canonical owner.
An existing explicit owner is immutable.

## Authority Invariants

- admission binding and new model Turn retain the same agent and attribution;
  current execution and Turn owner projections agree
- Conversation cannot resume a wait or task solely because interaction ids
  match
- exact wait and task-result admission retain their existing authority fences
- untrusted or external metadata cannot create or merge Conversation identity
- WorkItem scheduling and settlement fences remain unchanged
- owner reconstruction is deterministic across restart
- provider recovery binding equals its validated predecessor binding; this
  narrow rule does not change task-result or exact-wait reentry admission

## Evaluation

`ProjectionManifest.activation_owner` may report the new durable owner, but
Phase 1 must leave rendered system/context sections and provider lowering
unchanged. Candidate manifests are compared with the frozen Phase 0 baseline;
owner-label changes are expected only where admission now records Conversation.

Phase 2 requires separate authorization and must prove owner-thread selection,
single representation per Turn, direct-predecessor retention, and provider
behavior before retiring legacy prompt sections.
