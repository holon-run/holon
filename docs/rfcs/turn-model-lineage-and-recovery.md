---
title: RFC: Turn Model Lineage And Recovery
date: 2026-05-17
status: draft
---

# Turn Model Lineage And Recovery

This RFC defines how Holon should handle model switching, provider fallback,
provider continuation state, and automatic recovery across agent turns.

It extends, but does not replace:

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [OpenAI Remote Compaction Boundary](./openai-remote-compaction.md)
- [Extensible Model And Provider Configuration](./extensible-model-provider-configuration.md)

## 1. Problem Shape

Holon turns can contain multiple provider/tool rounds:

1. build the effective prompt and tool surface
2. call a provider
3. receive assistant output and tool calls
4. execute tools
5. call the provider again with accumulated turn state
6. repeat until the turn reaches a terminal outcome

Provider fallback currently sits under the `AgentProvider` boundary. A failed
provider attempt can advance to the next configured candidate inside the same
`complete_turn` call. That is useful for pre-output availability failures, but
it leaves an unclear boundary once a turn has already produced assistant output,
tool calls, or tool side effects.

The ambiguity matters because provider model changes are not just a different
text generator. They can change:

- tool schema shape, including freeform grammar versus JSON function tools
- provider-specific prompt-cache behavior
- native context-management policy
- OpenAI `previous_response_id` continuation state
- OpenAI remote compaction items with encrypted provider content
- retry and output-token behavior

If Holon switches model lineage in the middle of a logical turn, the next model
may be asked to continue from a conversation, tool surface, cache state, or
provider window that was produced for a different provider contract.

## 2. Goals

- keep one logical turn's provider contract stable after it starts producing
  state
- preserve fallback for pre-output provider availability failures
- make user-requested model switches predictable
- prevent encrypted remote compaction from crossing model lineage boundaries
- make provider recovery explicit and auditable
- avoid unpaired tool-call protocol state at turn boundaries
- keep durable runtime state provider-independent

## 3. Non-Goals

- do not remove provider fallback
- do not require every fallback candidate to expose the same native provider
  features
- do not make remote provider compaction a source of semantic memory
- do not replace turn-local compaction
- do not require an LLM summarization pass for recovery
- do not make the model the authority for whether a turn can be recovered

## 4. Terms

### Agent Turn

One runtime execution pass started from an external operator message, internal
message, timer, waiting-plane wake, or recovery message.

A turn may contain many provider/tool rounds. A turn owns the in-memory
provider conversation projection and reaches one terminal outcome.

### Provider Round

One provider request/response inside a turn. A provider round may produce text,
tool calls, token usage, diagnostics, and provider transport state.

### Model Lineage

The model-facing contract used for one provider round.

At minimum, a lineage includes:

- provider id
- model id
- transport contract
- endpoint kind
- tool schema lowering mode
- native provider feature set
- request controls such as reasoning effort and context-management policy

OpenAI continuation state should further include the request shape that guards
`previous_response_id` and provider-window replay.

### Turn Active Model

The first model lineage that successfully produces assistant/provider output
for a turn. After this is set, the turn is locked to that lineage.

### Side Effect Boundary

The point after which the turn has produced state that another model lineage
must not implicitly inherit inside the same turn.

The boundary is crossed when any of the following has happened:

- assistant text was accepted into the turn record
- a tool call was accepted into the turn record
- a tool result was produced
- a workspace or external side effect occurred
- provider continuation state was advanced
- provider remote compaction state was installed

### Recovery Turn

A new internal turn created after a locked model fails later in a turn. It
continues from durable Holon state rather than hidden provider state.

## 5. Proposed Semantics

### 5.1 User Model Switches Are Next-Turn Only

When an operator or configuration update changes the requested model while a
turn is running, Holon should record the requested change as pending. It should
not change the model lineage used by the active turn.

The pending model becomes eligible when the next agent turn is started.

Exception: if a turn has not yet sent a provider request, the new model may be
used for that turn because no provider state exists yet.

### 5.2 Fallback Is Allowed Before The Side Effect Boundary

Provider fallback may advance through configured candidates while no candidate
has successfully produced accepted turn state.

This covers:

- authentication or routing failures
- rate limits
- transient transport failures
- provider service failures
- unsupported endpoint failures before provider state is installed

The request can be rebuilt for each candidate from the same Holon prompt frame,
conversation projection, and provider-independent tool list. The fallback
provider may lower the shared tool surface to the intersection of candidate
capabilities.

### 5.3 First Successful Candidate Locks The Turn

Once a candidate successfully returns provider output that Holon accepts into
the turn, Holon records that candidate as `turn_active_model_ref` and its
derived lineage as the active lineage.

All later provider rounds in the same turn must use that locked lineage.

Retry is still allowed for the locked lineage according to provider retry
classification. Fallback to another lineage is not allowed after the lock.

### 5.4 Locked-Lineage Failure Terminates The Current Turn

If the locked lineage fails after retries are exhausted, the current turn should
terminate with an explicit provider-failure terminal kind.

The turn terminal record should include:

- requested model ref
- active model ref
- failed provider attempt timeline
- whether the side effect boundary was crossed
- whether a recovery turn was queued
- last accepted assistant text preview, if any

The current turn must not silently continue under another model lineage.

### 5.5 Recovery Uses A New Turn

Holon may automatically enqueue an internal recovery message after a locked
lineage fails. The recovery turn is a normal new turn:

- it rebuilds the effective prompt
- it rebuilds the tool schema
- it applies pending model selection
- it starts with no prior provider `previous_response_id`
- it does not replay encrypted remote compaction from the failed lineage
- it continues from durable transcript, working memory, work-item state, and
  workspace state

The recovery message should tell the model that the previous turn stopped
because the active provider failed and that it must continue from persisted
state without assuming hidden provider context.

Suggested recovery context:

```text
Runtime recovery: the previous turn stopped after the active provider failed.
Continue from the persisted transcript, current work item, and workspace state.
Do not assume hidden provider continuation state is still available. Do not
repeat completed tool work unless current evidence shows it is necessary.
```

Automatic recovery should be bounded. A repeated recovery failure should stop
and surface the failure to the operator instead of creating an infinite recovery
loop.

## 6. Provider State Rules

### 6.1 Continuation State Is Lineage-Scoped

Provider continuation state is private to a model lineage.

For OpenAI Responses and OpenAI Codex Responses, this includes:

- `previous_response_id`
- provider-window replay items
- remote compaction items
- encrypted `compaction` / `compaction_summary` content
- unsupported compact endpoint negative cache

Changing provider id, model id, transport contract, endpoint kind, request
controls, tool schema, or prompt frame should invalidate the provider
continuation state for the new lineage.

### 6.2 Encrypted Remote Compaction Is Not Portable

Encrypted provider content must not cross lineage boundaries.

Holon may store hashes, byte lengths, item counts, and request-shape metadata
for diagnostics. It must not treat encrypted content as semantic memory or pass
it to a different lineage as ordinary context.

### 6.3 Request Shape Fallback Is A Safety Net, Not The Contract

OpenAI provider-window replay currently compares request shape and falls back
to a full request when the shape changes. This is useful, but it should not be
the only model-switching guard.

The runtime should make lineage reset explicit so diagnostics can say:

- `lineage_changed`
- `pending_model_promoted`
- `locked_lineage_failure`
- `recovery_turn_started`

instead of only reporting a low-level request-shape mismatch.

## 7. Tool Protocol Rules

### 7.1 Tool Surface Is Stable Inside A Locked Turn

The tool schema exposed to the model should remain stable after the first
successful provider candidate is locked.

Fallback before the lock may use a shared lowered tool surface. If configured
candidates disagree on freeform grammar support, native web search, or context
management, the fallback provider should expose only the compatible surface.

### 7.2 No Unpaired Tool Calls Across Turn Boundaries

A recovery turn must not inherit a half-open provider protocol state.

Before terminating the failed turn, Holon should ensure durable records are
closed in one of these forms:

- tool call plus tool result
- tool call plus explicit cancellation/failure record
- assistant text without tool call
- provider failure before any accepted assistant/tool state

The recovery turn should see durable Holon facts, not a protocol fragment that
only the failed provider lineage could interpret.

### 7.3 Side Effects Are Resumed From Evidence

If tools already changed files, spawned processes, or touched external systems,
the recovery turn must continue from current evidence:

- workspace file state
- durable tool records
- work item and todo state
- transcript entries
- audit events

It should not repeat tool work just because the failed provider's hidden
continuation state is unavailable.

## 8. Runtime State Model

Suggested runtime additions:

```rust
struct TurnLineageState {
    requested_model_ref: ModelRef,
    pending_model_ref: Option<ModelRef>,
    active_model_ref: Option<ModelRef>,
    active_lineage_key: Option<ModelLineageKey>,
    side_effect_boundary_crossed: bool,
    recovery_attempt: u32,
}

struct ModelLineageKey {
    provider_id: ProviderId,
    model: String,
    transport: ProviderTransportKind,
    endpoint_kind: String,
    tool_surface_hash: String,
    request_controls_hash: String,
}
```

The exact shape can be smaller if the provider request shape already carries
some fields. The important property is that lineage-sensitive state has a
first-class identity and reset reason.

## 9. Implementation Plan

1. Add diagnostics-only lineage tracking to provider attempt timelines.
2. Add `turn_active_model_ref` to the turn loop and lock it on first accepted
   provider output.
3. Teach fallback to stop advancing to another candidate after a turn lock.
4. Add a terminal kind for locked-lineage provider failure.
5. Add optional bounded recovery-turn enqueueing.
6. Reset provider continuation explicitly when pending model selection is
   promoted at the next turn.
7. Add tests for:
   - fallback before any accepted output
   - no fallback after first successful provider round
   - recovery turn starts as a new turn
   - encrypted remote compaction is not replayed across lineage change
   - user model switch does not affect an already-running turn

## 10. Open Questions

- Should automatic recovery be enabled by default or gated by runtime config?
- What terminal kind name should be used for locked-lineage provider failure?
- Should recovery messages be operator-visible by default?
- Should a recovery turn count against the same run budget as the failed turn?
- Should fallback chains be grouped by compatible tool surface, or should the
  current all-candidate intersection remain the only exposed surface?

## 11. Decision

Holon should converge on this rule:

> One agent turn can use fallback before it produces accepted state. Once a
> model lineage has produced accepted state, the turn is locked to that lineage.
> Later provider failure ends the turn. Recovery, if enabled, starts as a new
> turn from durable Holon state.

This keeps provider fallback useful without letting hidden provider state,
encrypted remote compaction, or model-specific tool contracts leak across
semantic turn boundaries.
