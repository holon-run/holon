---
title: RFC: OpenAI Remote Compaction Boundary
date: 2026-04-30
status: draft
---

# OpenAI Remote Compaction Boundary For Holon

This RFC defines how Holon should use OpenAI Responses compaction without
weakening Holon's local memory, auditability, or provider-independent context
model.

It extends, but does not replace:

- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)

## 1. Problem Shape

OpenAI Responses exposes remote compaction through:

- standalone `POST /v1/responses/compact`
- server-side compaction through `/v1/responses` `context_management`

The compacted item returned by OpenAI includes encrypted provider state. Holon
can store and replay that item, but cannot inspect its semantic contents.

That creates three design constraints:

1. remote compaction cannot answer what the agent still remembers
2. local compaction can rewrite model-visible prompt prefixes and affect cache
3. repeated compactions can produce more than one provider compaction item

Holon should therefore treat remote compaction as a provider-window optimization,
not as Holon's source of semantic memory.

## 2. External API Facts

This design relies on the public OpenAI Responses API documentation:

- `POST /v1/responses/compact` returns a compacted input window for future
  Responses calls.
- The compacted provider state is represented as a `compaction` item with
  encrypted content.
- Applications should pass the compaction item back to OpenAI as-is.
- `previous_response_id` can chain responses, but previous input tokens in the
  chain are still billed as input tokens.
- In stateless input-array chaining, applications can drop items before the
  most recent compaction item. In `previous_response_id` chaining, applications
  should not manually prune the chain.

References:

- <https://developers.openai.com/api/docs/guides/compaction>
- <https://developers.openai.com/api/reference/resources/responses/methods/compact>
- <https://developers.openai.com/api/docs/guides/conversation-state>

## 3. Design Goal

Holon should use OpenAI remote compaction to keep the OpenAI provider window
small while preserving Holon's own inspectable runtime state.

The goal is not:

- to replace Holon's local context compaction
- to treat encrypted provider state as durable semantic memory
- to make OpenAI the only source of truth for resuming work
- to compact the same historical span twice in the same provider window

## 4. Two-Layer Compaction Model

Holon should keep two separate compaction layers.

### 4.1 Local Semantic Compaction

Local semantic compaction is owned by Holon.

It keeps a readable checkpoint of what matters for runtime continuity:

- objective and acceptance boundary
- active work item state
- current plan and unresolved risks
- changed files and relevant artifacts
- verification commands and outcomes
- important decisions and constraints

This layer is provider-independent and should remain the source for user-visible
debugging, resume, and audit.

### 4.2 OpenAI Provider-Window Compaction

OpenAI remote compaction is owned by the OpenAI transport.

It keeps provider-shaped history bounded:

- OpenAI input messages
- OpenAI assistant output items
- tool calls and tool outputs in OpenAI item shape
- OpenAI `compaction` items

This layer is not inspectable. Holon should store metadata about it, not infer
meaning from it.

Suggested metadata:

- compaction id
- trigger reason
- model and provider
- request shape hash
- covered provider item range
- number of compaction items in the replacement window
- token usage for the compaction pass
- hash and byte length of encrypted payloads

## 5. Cache And Continuation Rules

Compaction necessarily changes the model-visible prefix. That can reset or
reduce prompt-cache reuse for the compacted span.

This is acceptable only when the next requests become materially smaller.
Holon should avoid compaction churn by following these rules:

1. Do not local-compact and remote-compact the same provider-visible span in the
   same round.
2. After remote compaction, continue from the OpenAI compacted provider window
   as the canonical provider window.
3. Local semantic compaction may record a shadow checkpoint for observability,
   but it must not immediately rewrite the OpenAI provider window.
4. If Holon must rebuild the provider window from local semantic state, it
   should explicitly mark the OpenAI continuation state as reset.
5. Cache diagnostics should distinguish cache loss caused by intentional
   compaction from cache loss caused by unexpected request-shape drift.

Remote compaction should therefore be a threshold crossing event, not a routine
per-round rewrite.

## 6. Multiple Compaction Items

Holon must not assume there is only one encrypted compaction item.

Each OpenAI compaction item can carry its own encrypted content. A long-running
session may contain multiple compaction items, especially with server-side
compaction.

The OpenAI transport should represent provider state as:

```rust
struct OpenAiProviderWindow {
    items: Vec<serde_json::Value>,
    latest_compaction_index: Option<usize>,
}
```

The implementation may optimize around the latest compaction item, but the
stored provider window must support multiple `type: "compaction"` items.

## 7. Recommended Implementation Path

### 7.1 Phase 1: Continuation Diagnostics

Before adding remote compaction, fix observability for OpenAI continuation
misses.

For `conversation_not_strict_append_only`, record:

- previous expected prefix length
- current provider input length
- first mismatch index
- item type and stable id on both sides
- redacted previews or hashes
- request shape hash

This should explain whether continuation is failing because of provider item
reconstruction, local prompt projection, tool output grouping, or request-shape
changes.

### 7.2 Phase 2: Provider-Shaped Continuation Window

The OpenAI transport should stop relying on semantic transcript reconstruction
as the only basis for provider continuation.

Keep an OpenAI-shaped provider window:

- last response id
- request shape hash
- sent input items
- returned output items
- pending incremental input items
- compaction items

Use `previous_response_id` only when the OpenAI-shaped window is still valid.
If the semantic runtime rebuilds the provider window, reset continuation
explicitly.

### 7.3 Phase 3: Standalone Remote Compaction

Implement standalone `/responses/compact` first because it gives Holon an
explicit compaction boundary.

At a stable round boundary:

1. build the OpenAI-shaped provider window
2. call `/responses/compact`
3. validate the returned replacement window
4. store the replacement as the OpenAI provider window
5. record a local semantic shadow checkpoint
6. reset or refresh continuation state according to the returned window

The returned compaction payload should be treated as opaque and replayed as-is.

### 7.4 Phase 4: Server-Side Compaction

Only after standalone compaction is stable should Holon consider server-side
OpenAI compaction through `context_management`.

This mode is more coupled to `previous_response_id` chaining. Holon should not
manually prune provider history in this mode unless the OpenAI API explicitly
returns a replacement window boundary that Holon owns.

## 8. Runtime Trigger Policy

Remote compaction should be budget-driven.

Suggested initial triggers:

- provider input estimate exceeds a soft limit
- provider rounds exceed a benchmarked loop threshold
- OpenAI returns a context-window error and the provider window is compactable

Suggested non-triggers:

- short sessions
- first few rounds
- after every local semantic compaction
- while a tool call and its output are not paired yet
- when request shape changed for unrelated reasons

## 9. Benchmark Expectations

For issue-driven benchmarks such as `#787`, a successful implementation should
show:

- fewer full OpenAI requests after the first round
- fewer `conversation_not_strict_append_only` fallbacks
- lower total provider input tokens after compaction thresholds are crossed
- explicit diagnostics when cache is reset by compaction
- retained local readable checkpoints explaining the current task state

Remote compaction should be evaluated as a provider-cost and context-window
optimization. Local semantic compaction should be evaluated as an auditability
and resume-quality feature.

## 10. Open Questions

- Should Holon expose remote compaction metadata in benchmark summaries by
  default?
- Should local semantic shadow checkpoints be written only on remote compaction,
  or also on continuation reset?
- What is the first safe token threshold for `gpt-5.3-codex-spark` in live
  benchmark runs?
- Should server-side compaction be opt-in until standalone compaction has enough
  benchmark evidence?
