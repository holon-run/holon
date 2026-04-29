---
title: RFC: Long-Lived Context Memory
date: 2026-04-23
status: draft
---

# RFC: Long-Lived Context Memory

## Summary

Holon is designed around one durable agent with long-lived working memory.
The runtime should not require an operator-visible "new session" reset to keep
an agent useful.

This RFC covers cross-turn memory and prompt assembly. In-turn provider
conversation compaction inside one `run_agent_loop()` is handled separately by
`docs/rfcs/turn-local-context-compaction.md`.

This RFC defines the context compression and memory model that lets a Holon
agent continue across many turns while keeping prompt growth bounded.

The core design is:

- keep append-only durable logs as the audit source
- derive a deterministic working memory snapshot after turns
- accumulate active work into episode builders
- finalize immutable episode records at meaningful boundaries
- assemble each prompt from budgeted memory sections instead of replaying all
  history
- keep provider prompt-cache identity aligned with slow-changing memory
  revisions

## Problem

Long-lived coding agents accumulate context quickly:

- messages grow without bound
- command and tool output can dominate token usage
- result briefs and waiting state can become stale
- repeated free-form summaries drift over time
- rewriting large prompt prefixes harms provider cache reuse

A naive compaction strategy that summarizes old transcript text into one flat
blob is not sufficient for Holon because the agent is expected to stay useful
for a long time.

Holon needs memory that preserves continuity without treating the raw
conversation transcript as the only source of truth.

## Goals

- Preserve the current work state across many turns.
- Keep prompt assembly within a configurable estimated token budget.
- Prefer deterministic memory extraction from runtime evidence.
- Avoid repeated model-generated summaries as the primary memory primitive.
- Make older work searchable and selectable without rendering all of it.
- Keep prompt-cache behavior stable when memory has not materially changed.
- Preserve append-only logs for audit and reconstruction.

## Non-Goals

- This RFC does not define semantic-vector retrieval.
- This RFC does not introduce an operator-facing agent-memory reset mechanism.
- This RFC does not require a dedicated model call for compaction.
- This RFC does not make raw transcript history disappear from durable storage.
- This RFC does not define cross-agent memory sharing.
- This RFC does not define turn-local provider conversation compaction inside a
  single runtime turn.

## Memory Layers

Holon context memory has four layers.

### Durable Ledger

The durable ledger is the append-only source of truth.

Examples include:

- messages
- briefs
- tool executions
- transcript entries
- tasks
- work items
- work plans
- waiting intents
- context episode records
- working memory deltas

The ledger is not prompt-bounded. Compression changes the model-visible
projection, not the audit trail.

### Hot Turn Context

Hot turn context is rebuilt every prompt assembly.

It contains volatile, recent, directly actionable information:

- current input
- continuation context
- active work item
- active work plan
- queued and waiting work items
- recent messages
- recent briefs
- recent tool executions
- latest result brief
- current execution environment projection

Hot turn context is `TurnScoped` and should be aggressively budgeted.

### Working Memory

Working memory is the compact current-state projection of the agent.

It answers:

- what work is active or currently anchored?
- what is the delivery target?
- what scope constraints matter?
- what plan is current?
- which files are in the working set?
- what decisions are still relevant?
- what follow-ups remain?
- what is the agent waiting on?

The runtime stores this as `WorkingMemoryState.current_working_memory`, with a
monotonic `working_memory_revision`.

Working memory is derived from durable runtime state, not authored as a
free-form model summary.

### Episode Memory

Episode memory is the archive of completed coherent work chunks.

An active episode builder accumulates relevant evidence while work progresses:

- active work item id
- delivery target
- work summary
- scope hints
- touched files
- commands
- verification evidence
- decisions
- carry-forward follow-ups
- waiting state

When a boundary is reached, the runtime finalizes the builder into an immutable
`ContextEpisodeRecord` and appends it to durable storage.

Archived episodes are selected into prompt context by relevance and budget.
They are not all rendered by default.

## Lifecycle

Context memory changes at turn boundaries.

### Post-Turn Refresh

After a turn reaches closure, the runtime should:

1. derive the next `WorkingMemorySnapshot`
2. compare it with the previous snapshot
3. append a `WorkingMemoryDelta` when the snapshot changed
4. update `working_memory_revision`
5. keep the delta pending until it has been rendered into a prompt
6. merge the turn evidence into the active episode builder
7. finalize the active episode if a boundary was crossed

This keeps memory extraction deterministic and tied to runtime evidence.

### Episode Boundaries

The runtime should finalize an active episode when one of these boundaries is
crossed:

- active work switched
- wait boundary
- task rejoined
- result checkpoint
- hard turn cap

Episode boundaries should represent coherent work chunks, not arbitrary message
counts.

## Prompt Projection

Prompt assembly should build a fresh projection every turn, but each section
should have explicit stability and budget behavior.

The preferred order is:

1. stable system and policy prompt
2. `AgentScoped` working memory
3. selected `AgentScoped` relevant episode memory
4. `TurnScoped` pending working-memory delta
5. `TurnScoped` active work item and current plan
6. `TurnScoped` queued and waiting work items
7. `TurnScoped` recent messages, briefs, tools, and latest result
8. current input and continuation context

Working memory should be rendered whenever it is non-empty.

The pending working-memory delta should be rendered only until the runtime has
confirmed that the target revision has appeared in a prompt. After that it can
be cleared.

The legacy `context_summary` may remain only as fallback when structured
working memory is empty.

## Budgeting

Prompt assembly should be budget-aware.

Each section is estimated before insertion. Lower-priority sections should be
omitted or truncated before displacing higher-value state such as:

- current input
- active work item
- working memory
- selected relevant episode memory
- pending memory delta

The runtime should reserve budget for current input before adding memory
sections.

Episode memory selection should consider both relevance and recency, then render
only the top records that fit the remaining budget.

## Provider Cache Behavior

Long-lived memory should be cache-friendly.

Holon should keep separate prompt stability categories:

- `Stable`
- `AgentScoped`
- `TurnScoped`

The prompt cache identity should include:

- agent id
- agent prompt cache key
- working memory revision
- compression epoch

`working_memory_revision` changes when current agent working memory changes.
`compression_epoch` changes when archived episode memory or fallback compaction
changes the stable context shape.

Provider integrations should use the provider-specific cache mechanism without
requiring the rest of the runtime to know provider details.

For providers with explicit cache-control blocks, stable and agent-scoped
boundaries may be marked as cache breakpoints.

For providers with prompt-cache keys, the key should remain stable unless the
memory revision or compression epoch changes.

## Compaction Strategy

Holon should prefer structured deterministic memory over model-generated
compaction.

The primary compaction path is:

1. keep raw durable logs append-only
2. derive working memory from durable state
3. accumulate active episode evidence
4. archive completed episodes immutably
5. select only relevant episodes into the prompt
6. trim recent hot context by budget

Fallback message compaction may still exist for migration or empty-memory cases,
but it must not be the primary continuity mechanism.

## Model Calls

The baseline memory model does not require a dedicated LLM call to compact
context.

Long content should first be compressed through deterministic projections:

- tool summaries
- result briefs
- work item summaries
- work plans
- touched file lists
- commands and verification records
- episode summaries derived from structured fields

An optional future LLM summarizer may be introduced for specific oversized
artifacts, but it should write bounded evidence records and should not replace
the deterministic working memory model.

## Runtime Contract

The agent should see memory as prompt text, not as hidden runtime state.

If the runtime updates memory after a turn, the next prompt must expose:

- the new `working_memory`
- a compact `working_memory_delta` describing what changed

This avoids the failure mode where runtime state changes but the model has no
observable signal that its memory projection changed.

## Acceptance Criteria

- A long-running agent can continue after old transcript tail is omitted.
- Current work item, plan, waiting state, and relevant evidence remain visible.
- Working memory changes produce durable deltas.
- Episode records are finalized at meaningful boundaries.
- Prompt assembly respects an estimated token budget.
- Provider prompt cache identity does not churn when memory is unchanged.
- Legacy flat `context_summary` is not the main continuity mechanism.

## Related Implementation

The current implementation maps to this RFC through:

- `WorkingMemoryState`
- `WorkingMemorySnapshot`
- `WorkingMemoryDelta`
- `TurnMemoryDelta`
- `ActiveEpisodeBuilder`
- `ContextEpisodeRecord`
- `refresh_working_memory`
- `refresh_episode_memory`
- `build_context`
- provider prompt-cache identity and stability markers
