---
title: RFC: Operator Interjection Safe Points
date: 2026-05-20
status: draft
Handle: rfc-operator-interjection-safe-points
---

# Operator Interjection Safe Points

This RFC defines how Holon should admit operator input while a turn is already
running, especially when the active provider round has produced tool calls that
have not yet been executed.

It extends:

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md)
- [Turn Model Lineage And Recovery](./turn-model-lineage-and-recovery.md)
- [Remote Operator Transport and Delivery](./remote-operator-transport-and-delivery.md)
- [Tool Result Envelope](./tool-result-envelope.md)

## 1. Problem Shape

Holon admits trusted operator prompts with interjection priority while a turn is
running. The scheduler owns classification and queue status, while the turn loop
drains interjections at provider/tool safe points.

The current implementation has an ambiguous safe point before tool execution:

1. a provider round returns `tool_use` blocks;
2. an operator prompt is admitted before those tools execute;
3. the turn loop records a synthetic round using only assistant text blocks;
4. if the provider returned tool calls with no text, the provider-visible
   conversation can contain an empty assistant message.

That shape violates provider contracts such as Anthropic messages, where every
message must have non-empty content. More importantly, it is semantically wrong:
the assistant did not produce an empty message, it produced tool calls that were
interrupted by later operator input.

## 2. Goals

- preserve provider-visible transcript validity across all interjection paths;
- preserve side-effect evidence and queue status for admitted interjections;
- keep ordinary operator interjection distinct from explicit cancel/abort;
- avoid dropping provider-produced tool calls after they have been accepted;
- keep tool-call and tool-result protocol pairs complete when tool calls are
  visible to the next provider round;
- make interjection boundaries explicit in audit events and tests.

## 3. Non-Goals

- do not define a full approval workflow;
- do not make every operator prompt abort the active turn;
- do not require provider requests already in flight to be cancelled;
- do not expose audit-only runtime events as assistant content;
- do not let remote operator transport define different safe-point semantics
  from local operator input.

## 4. Terms

### Operator Interjection

A trusted `operator_prompt` that is admitted while a turn is running and is
eligible to become model-visible input before the turn reaches its normal
terminal outcome.

Operator interjection is an append operation by default. It is not an implicit
run abort, tool cancellation, or transcript rewrite.

### Safe Point

A turn-loop boundary where queued operator interjections may be drained and
made model-visible without corrupting provider protocol state.

### Pending Tool Calls

Tool calls returned by the provider in the current assistant round that have
been parsed and accepted by the runtime, but whose tools have not yet produced
results.

### Provider-Visible Transcript

The conversation projection sent to a model provider. It must satisfy the target
provider contract and must not include audit-only state that has no valid
provider representation.

## 5. Safe-Point Contract

### Before Provider Request

Queued operator interjections may be included as normal user/operator input
before the next provider request is built.

### During Provider Request

Ordinary operator interjections do not cancel a provider HTTP request that is
already in flight. The runtime may record that input arrived while the request
was running, but model-visible handling waits until a later safe point.

Explicit abort/cancel control actions are a separate contract and may cancel
the current run if the lifecycle control layer authorizes them.

### After Provider Round With No Tool Calls

If the provider round has no pending tool calls, admitted operator interjections
may be appended as follow-up user text for the next provider round inside the
same turn.

If the assistant round has no model-visible content and no tool calls, it must
not be projected as an empty assistant message.

### After Provider Round With Pending Tool Calls

If the provider round has pending tool calls, ordinary operator interjections
must not discard those tool calls.

The default behavior is:

1. keep the assistant tool-call round intact;
2. execute the pending tools;
3. record the interjection as admitted at the `before_tool_execution` boundary;
4. append the rendered operator interjection after the corresponding tool
   results as follow-up user text for the next provider round.

The next provider-visible projection should therefore have this shape:

```text
assistant: tool_use ...
user: tool_result ...
user: [Operator message received while this turn was in progress] ...
```

This preserves the provider protocol pair while still giving the operator input
priority before the next provider decision.

### After Tool Results

Operator interjections admitted after tool results are appended as follow-up
user text in the same round. They should be visible to the next provider request
after the tool results.

### Explicit Cancel Or Abort

Skipping pending tool calls is only valid for an explicit cancel or abort action,
not for an ordinary operator interjection.

If a future cancel action needs to abandon pending tool calls after they were
accepted from a provider, the provider-visible transcript must not contain
unpaired tool calls. Either the abandoned assistant round stays audit-only, or
the runtime emits a provider-valid cancellation representation with clear
provider support. The default should be audit-only until a provider-safe
cancellation representation exists.

## 6. Transcript Invariants

The provider-visible transcript must satisfy these invariants:

- no empty assistant message;
- no empty user message;
- no `assistant tool_use` visible without corresponding `user tool_result`
  unless the provider explicitly supports that continuation shape;
- no audit-only runtime event masquerading as assistant output;
- no loss of operator provenance in the rendered interjection text;
- no silent conversion of ordinary interjection into abort/cancel semantics.

The durable transcript and audit ledger may record richer evidence, including
interjection boundary, message id, admission context, delivery surface, and
pending tool-call counts. That evidence is not automatically provider-visible.

## 7. Current Implementation Notes

The current code has three relevant anchors:

- `scheduler::is_operator_interjection_message` classifies trusted operator
  prompts that may interject into a running turn.
- `TurnExecution::drain_operator_interjections` pops those messages, records
  `QueueEntryStatus::Interjected`, appends incoming transcript entries, and
  emits `operator_interjection_admitted`.
- The turn loop currently drains interjections at `after_provider_round`,
  `before_tool_execution`, and `after_tool_results`.

The `before_tool_execution` branch should change. Today it constructs a
synthetic round from `text_blocks` only and clears pending tool calls. When the
assistant returned only tool calls, this creates an empty assistant block list
that later becomes an invalid provider message.

The intended implementation is:

- drain and record the interjection at `before_tool_execution`;
- continue executing the accepted pending tool calls;
- store the drained interjections on the eventual round record together with
  the tool results;
- make the next provider request see tool results first and interjection text
  second.

As a defensive layer, provider request lowering should also reject or skip empty
provider-visible messages before sending a request. Runtime projection should be
the primary fix; transport-level validation is a guardrail.

## 8. Required Verification

Add regression coverage for:

- a provider round that returns only tool calls;
- a trusted operator prompt admitted at `before_tool_execution`;
- the tools still execute;
- the queue entry is recorded as `Interjected`;
- `operator_interjection_admitted` records boundary `before_tool_execution`;
- the next provider request includes assistant `tool_use`, user `tool_result`,
  and the rendered operator interjection;
- no empty assistant or user message is sent to Anthropic-compatible providers.

Provider-level tests should include Anthropic messages lowering because that
contract rejects empty message content.

Scheduler tests should continue to assert classification and queue status, but
the transcript-shape invariant belongs to turn/runtime provider-projection
tests.
