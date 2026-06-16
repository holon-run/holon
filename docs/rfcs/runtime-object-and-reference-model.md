---
title: RFC: Runtime Object and Reference Model
date: 2026-06-16
status: draft
Handle: rfc-runtime-object-and-reference-model
---

# RFC: Runtime Object and Reference Model

## Summary

Holon runtime records should have explicit ownership boundaries. Every durable
object should answer one question, and derived records should reference that
object rather than copying its full payload.

The target model is:

```text
MessageEnvelope      = admitted input truth
TranscriptEntry      = provider-visible conversation spine
ToolExecutionRecord  = tool execution evidence truth
BriefRecord          = user-facing summary and delivery truth
TurnRecord           = relationship spine
AuditEvent           = cursorable audit/debug/event-stream mirror
```

This RFC defines the source-of-truth boundary for those objects, the references
between them, and the payload duplication that should be removed before adding
larger read caches or batch hydration layers.

## Related Documents

- [Runtime Ledger Files and Relations](./runtime-ledger-files-and-relations.md)
- [Runtime Ref Resolution and MemoryGet](./runtime-ref-resolution-and-memory-get.md)
- [Turn-Based Context Projection](./turn-based-context-projection.md)
- [Turn Model Lineage And Recovery](./turn-model-lineage-and-recovery.md)
- [Tool Result Envelope](./tool-result-envelope.md)
- [Event Stream Interface Design](./event-stream-interface.md)
- [Operator Display Levels and Event Presentation](./operator-display-levels-and-event-presentation.md)
- [Operator Interjection Safe Points](./operator-interjection-safe-points.md)
- [Work Item Centered Agent Runtime](./work-item-centered-agent-runtime.md)

## Problem

Holon has progressively slimmed events and moved toward object references.
That solved large event payloads, but it exposed a deeper object-model issue:
several durable objects still copy each other's content.

Current examples include:

- incoming transcript entries have a `related_message_id`, but also duplicate
  message body, origin, metadata, delivery surface, correlation, and causation;
- transcript-backed briefs can point at a transcript entry while still storing
  the same full user-facing text in `BriefRecord.text`;
- transcript tool-result entries can copy provider-visible tool result blocks
  while `ToolExecutionRecord` stores the tool execution input/output;
- some prompt, TUI, memory, and presentation paths still treat copied previews
  as if they were canonical bodies.

Without a shared object boundary, new cache layers can accidentally preserve
the duplication by caching multiple representations of the same payload. Batch
hydration also becomes ambiguous: callers need to know which object is
authoritative and which object is only a link, preview, or replay spine.

## Goals

- Define canonical ownership for admitted input, assistant output, tool
  execution evidence, user-facing summaries, turn relations, and audit events.
- Ensure large payloads have one canonical owner.
- Define which object references are stable and how derived objects should
  point to canonical data.
- Keep provider replay requirements separate from tool execution evidence.
- Keep user-facing delivery summaries separate from raw transcript or tool
  payloads.
- Give prompt, state, event, TUI, memory, and future cache layers one common
  boundary.
- Define a staged migration path that keeps historical data readable.

## Non-Goals

- Do not define every database table column.
- Do not require immediate migration of historical records.
- Do not remove bounded previews from records that need display or ranking.
- Do not make audit events canonical state.
- Do not make the memory index a source of truth.
- Do not define a cross-process cache-coherence protocol. This RFC assumes one
  daemon owns runtime mutation for a database.

## Terms

### Canonical Object

A canonical object owns a durable fact or payload. Other records may reference,
preview, index, or summarize it, but they should not duplicate its full content.

### Derived Object

A derived object stores a projection, summary, replay relation, or display
shape computed from canonical objects. It may be durable when the projection is
itself useful, but it must identify its source objects.

### Preview

A preview is bounded text for display, ranking, or prompt triage. It is not a
replacement for the canonical body.

### Provider-Visible Shape

Provider-visible shape is the exact or bounded block structure that Holon sends
back to a model provider during conversation replay or continuation. It is not
the same thing as full tool execution output.

## Canonical Objects

### MessageEnvelope

`MessageEnvelope` is the canonical admitted input object.

It owns:

- message id and sequence;
- target agent id;
- creation time;
- kind;
- origin;
- authority class;
- priority;
- trigger kind;
- body;
- delivery surface;
- admission context;
- correlation and causation ids;
- source refs derived from trusted runtime metadata.

It should be used for input from:

- trusted operator messages;
- external channels and webhooks;
- callbacks;
- timers;
- task results;
- runtime/system wake messages.

Transcript, turn, event, queue, and prompt records may reference a message by
id, but they should not copy its full body unless they are preserving legacy
data.

### TranscriptEntry

`TranscriptEntry` is the provider-visible conversation spine.

It owns model-facing output and replay structure, not admitted input truth or
tool execution truth.

Transcript kinds should follow these boundaries:

- `IncomingMessage` references a `MessageEnvelope`; it should not duplicate the
  full message body or metadata.
- `AssistantRound` owns provider-visible assistant blocks, provider stop
  reason, round number, token usage, and provider attempt diagnostics needed
  for recovery and debugging.
- `ToolResults` owns the provider replay mapping between assistant tool calls
  and runtime tool executions. It should not own full tool output.
- `ContinuationPrompt`, `RuntimeFailure`, `SubagentPrompt`, and
  `SubagentAssistantRound` own provider-visible or runtime-generated
  conversation material when no other canonical object owns that text.

`TranscriptEntry` is allowed to store provider-visible text when the model saw
that text. It should store refs when the model saw a ref notice, bounded
summary, or artifact handle.

### ToolExecutionRecord

`ToolExecutionRecord` is the canonical tool execution evidence object.

It owns:

- tool execution id;
- tool name;
- authority class;
- invocation surface;
- input;
- output;
- status;
- duration;
- summary;
- work item and turn binding.

Transcript entries and audit events may reference a tool execution id, but they
should not copy full tool input/output. Full command output, stdout, stderr,
artifact references, and structured tool results belong here or in explicitly
referenced artifacts.

### BriefRecord

`BriefRecord` is the canonical user-facing summary or delivery object.

It owns:

- brief id;
- agent and workspace binding;
- work item binding;
- turn binding;
- brief kind;
- user-facing delivery text or summary text;
- delivery attachments when they are user-facing;
- source references such as message, task, or transcript entry ids.

Briefs are not raw input and should not replace `MessageEnvelope`. A normal
admission acknowledgement is lifecycle evidence, not semantic result delivery.

There are two content modes:

- inline brief: `BriefRecord` owns `text`;
- transcript-backed brief: `BriefRecord` references transcript content and may
  store only a bounded preview or summary once resolvers exist.

Historical rows may continue to store full text for compatibility. New code
should treat `content_source` as the first-class source contract rather than a
decorative field.

### TurnRecord

`TurnRecord` is the relationship spine for one runtime activation.

It owns:

- turn id and index;
- run id;
- current work item id at the turn boundary;
- trigger summary;
- input message ids;
- tool execution ids;
- produced brief ids;
- delivery summary ids;
- completed work item ids;
- waiting condition ids;
- terminal summary.

It does not own full message bodies, tool outputs, transcript blocks, or brief
text. Prompt projection should join through refs when it needs those bodies.

### AuditEvent

`AuditEvent` is a cursorable audit/debug/event-stream mirror.

It owns:

- event id and sequence;
- event kind;
- timestamp;
- event-specific lightweight metadata.

It must not be treated as canonical domain state. Events should reference
canonical objects by id and include bounded previews only when needed for
operator display or diagnostics.

## Relationship Model

The preferred durable graph is:

```text
MessageEnvelope
  <- TurnRecord.input_message_ids
  <- TranscriptEntry.related_message_id for incoming input
  <- BriefRecord.related_message_id when a result answers an input

TranscriptEntry(AssistantRound)
  <- BriefRecord.content_source when a brief is derived from assistant output

ToolExecutionRecord
  <- TurnRecord.tool_execution_ids
  <- TranscriptEntry(ToolResults).results[*].tool_execution_id
  <- memory/source refs such as tool_execution:<id>:output

BriefRecord
  <- TurnRecord.produced_brief_ids
  <- WorkItemRecord.result_brief_id
  <- AuditEvent(brief_created).brief_id

TurnRecord
  <- prompt recent_turns projection
  <- turn: refs
```

Objects may also carry work item, task, workspace, model, and wait refs when
those refs are part of the object's domain relation.

## Reference Rules

1. Full payload has one owner.

   If a record can point to another canonical object, it should store the ref
   and at most a bounded preview.

2. Refs must be stable.

   A ref in prompt, transcript, event, memory, or TUI state should remain
   resolvable for the life of the underlying runtime data.

3. Derived records must be explicit.

   A projection must expose enough metadata to tell whether a field is a
   canonical body, a preview, or a ref notice.

4. Events are not evidence bodies.

   Events should be small and cursorable. Full objects should be fetched
   through object APIs or runtime refs.

5. Prompt-visible refs must be dereferenceable.

   Any ref rendered into prompt context should have a planned or implemented
   resolver.

6. Provider-visible replay is distinct from execution evidence.

   The transcript may record what the model saw, but `ToolExecutionRecord`
   owns the full result.

7. Runtime object refs are not agent-qualified by default.

   Runtime object ids are globally unique enough for Holon's runtime scope.
   Stored refs should stay short:

   ```text
   message:<message_id>
   transcript:<entry_id>
   brief:<brief_id>
   turn:<turn_id>
   tool_execution:<tool_execution_id>:output
   ```

   The referenced object still stores its owning `agent_id` as metadata. Access
   control and parent/child visibility policy are outside this RFC; those rules
   belong to the runtime/API boundary that resolves or exposes the stored ref.

   Qualified refs should be reserved for namespaces whose ids are not globally
   unique or for external systems where the namespace itself requires a scoped
   identifier.

## Transcript Rules

### Incoming Messages

`TranscriptEntry::IncomingMessage` should become a lightweight transcript
marker:

```json
{
  "kind": "incoming_message",
  "related_message_id": "msg_...",
  "data": {
    "message_ref": "message:msg_...",
    "delivery_surface": "http_webhook",
    "admission_context": "public_unauthenticated"
  }
}
```

The `MessageEnvelope` remains the source for kind, origin, authority, body,
metadata, correlation, and causation.

Small provenance fields may remain on the transcript marker if they are needed
for fast display or historical debugging, but they should be treated as a
snapshot, not the canonical message.

### Assistant Rounds

`TranscriptEntry::AssistantRound` owns the assistant blocks that the provider
returned and Holon accepted as the assistant side of the conversation.

It may include:

- block list;
- round number;
- stop reason;
- token usage;
- provider cache usage;
- prompt cache identity;
- provider request diagnostics;
- model lineage attempt metadata.

Briefs may reference an assistant round when their text is derived from that
assistant output.

### Tool Results

`TranscriptEntry::ToolResults` should store provider replay mapping, not full
tool output.

Target shape:

```json
{
  "results": [
    {
      "tool_call_id": "call_...",
      "tool_execution_id": "tool_...",
      "content_ref": "tool_execution:tool_...:output",
      "provider_visible_text": "short bounded result or ref notice",
      "status": "success",
      "truncated": true
    }
  ]
}
```

The transcript owns:

- `tool_call_id`, because it is provider conversation identity;
- ordering of returned tool result blocks;
- the provider-visible text or ref notice;
- truncation and replay metadata.

`ToolExecutionRecord` owns:

- tool input;
- full output;
- stdout and stderr;
- artifact refs;
- execution status and duration.

If `TurnRecord.tool_execution_ids` plus tool records later provide enough
ordered replay metadata, `TranscriptEntry::ToolResults` may become optional.
Until then, it remains the correct place for the provider-visible mapping.

## Brief Rules

### Inline Briefs

An inline brief owns its `text`.

This mode is appropriate when the brief text is generated directly as the
user-facing delivery object and does not already live as assistant transcript
content.

### Transcript-Backed Briefs

A transcript-backed brief references an assistant transcript entry through
`content_source`.

`BriefRecord.text` remains present during this RFC's migration. Its meaning
depends on source:

- for `BriefContentSource::Inline`, `text` is the canonical full brief text;
- for `BriefContentSource::TranscriptEntry`, `text` is a bounded preview or
  compatibility copy, not the canonical full text.

Target direction:

```json
{
  "id": "brief_...",
  "kind": "result",
  "content_source": {
    "kind": "transcript_entry",
    "entry_id": "tr_..."
  },
  "text_preview": "bounded preview"
}
```

The full text should be resolved from the transcript entry when needed for:

- prompt rendering;
- operator display;
- memory indexing;
- work item completion report display.

During migration, `BriefRecord.text` may continue storing full text. Consumers
should gradually move behind a brief content resolver so the field can later
become preview-only or optional for transcript-backed briefs.

`finalizes_assistant_round_id` should be folded into the content-source model
over time. The target source shape is:

```text
BriefContentSource::TranscriptEntry {
  entry_id,
  relation,
}
```

Where `relation` can distinguish at least:

- `derived_from`;
- `finalizes`;
- `excerpt`.

Until that migration happens, writers may populate both
`content_source = TranscriptEntry { entry_id }` and
`finalizes_assistant_round_id = Some(entry_id)` for compatibility.

### Completion Reports

When a WorkItem completion report is promoted from assistant output, the brief
should be linked to:

- the completed work item id;
- the turn id;
- the assistant transcript entry that supplied the report;
- the `WorkItemRecord.result_brief_id`.

The work item should reference the result brief rather than duplicating the
full completion report. A bounded `result_summary` may remain as a current-state
preview when needed for fast state or list projection.

## Prompt, State, Event, And Memory Consumption

### Prompt Context

Prompt context should consume:

- current runtime projection from in-memory runtime projection caches where
  available;
- turn spine from `TurnRecord`;
- input bodies from `MessageEnvelope`;
- assistant output from `TranscriptEntry::AssistantRound`;
- tool execution details from `ToolExecutionRecord`;
- user-facing summaries from `BriefRecord` through a resolver;
- refs and bounded previews rather than copied payloads.

Prompt rendering should not require every object to be fully loaded when a ref
and preview are enough.

### Agent State API

Agent state snapshots should render current runtime projection from memory
during normal daemon operation.

The state API should not embed historical transcript, tool output, brief text,
or message bodies unless the route explicitly asks for that object.

### Event Streams

Event payloads should remain lightweight and carry ids plus bounded previews.

Clients should hydrate full objects through dedicated endpoints or batch object
APIs.

### Memory Indexing

The memory index may index selected object content, but it is not canonical.

Index entries should store source refs and snippets. `MemoryGet` or object APIs
should resolve source refs through the authoritative object stores.

## Cache And Batch Hydration Implications

Read caches should cache canonical objects and resolver results, not multiple
duplicated full payloads.

Recommended cache layers:

- current runtime projection cache for `/state`, scheduler closure, and prompt
  current-work sections;
- evidence read-through cache for by-id message, transcript, tool execution,
  brief, and turn lookups;
- batch APIs for hydrating lists of canonical object ids.

Caches should not hide object ownership. A cached transcript marker that points
to a message should not become a second cached message body.

## Migration Plan

### Phase 1: Document And Add Resolvers

- Add a brief content resolver that understands inline and transcript-backed
  briefs.
- Add or formalize message, transcript, tool execution, brief, and turn
  resolvers for prompt/TUI/memory consumers.
- Add trace metrics for resolver reads and payload sizes.

### Phase 2: Slim Incoming Transcript Entries

- Change new `IncomingMessage` transcript entries to store `related_message_id`
  and bounded provenance only.
- Update consumers to resolve message body from `MessageEnvelope`.
- Keep legacy transcript rows readable.

### Phase 3: Slim Transcript-Backed Briefs

- Route prompt, presentation, memory indexing, and work item completion report
  display through the brief resolver.
- For transcript-backed briefs, store preview/summary instead of full text when
  the source transcript is available.
- Keep inline briefs unchanged.

### Phase 4: Ref-Back Tool Result Transcript Entries

- Ensure each provider tool call records a stable mapping to a
  `ToolExecutionRecord`.
- Store provider-visible bounded result/ref notices in transcript.
- Remove full duplicated tool result output from transcript entries.
- Keep `ToolExecutionRecord` as the full output owner.

### Phase 5: Tighten Events And Indexes

- Audit event payloads for remaining full-object copies.
- Avoid storing full payload copies in secondary indexes unless required by the
  index implementation.
- Add tests that assert large object bodies are not duplicated into transcript
  or event payloads.
- Treat secondary-index `payload_json` copies as transitional unless the index
  implementation requires a covering copy. Indexes should prefer object id,
  agent id, searchable text, kind, preview, created time, sequence, and ranking
  metadata. Full payload resolution should go back to the canonical object
  table.

## Compatibility

Historical records may continue to contain duplicated data. Readers must remain
able to decode them.

Migration should be forward-only:

- new writes follow the object boundary;
- old rows are interpreted through compatibility resolvers;
- APIs expose stable object contracts rather than raw storage implementation
  details.

## Decisions

### Brief Text

`BriefRecord.text` stays in the record for now. It is canonical only for inline
briefs. For transcript-backed briefs, it is a bounded preview or compatibility
copy. Consumers should use a brief content resolver before relying on full
text.

### Brief Source Relation

The source relation should eventually replace the separate
`finalizes_assistant_round_id` field. The migration should keep old readers
working by writing both fields until the resolver and consumers use the richer
source relation.

### Tool Result Transcript Entries

`TranscriptEntry::ToolResults` remains durable because it owns provider replay
mapping and provider-visible ordering. It should be ref-backed and bounded,
not a full copy of tool output.

If future `TurnRecord` and `ToolExecutionRecord` data can completely derive
ordered provider replay shape, `ToolResults` may become optional. That is not
the current target.

### Runtime Ref Scope

Runtime object refs do not include `agent_id` for uniqueness. Object ids are
already globally unique enough for runtime refs. The owning `agent_id` remains
on the canonical object row and can be used by higher-level APIs or resolvers
without lengthening the stored ref.

List queries, search results, and debug responses may include `agent_id` as
metadata, but refs should remain short.

### Secondary Index Payload Copies

Secondary indexes should not become canonical object stores. Full payload
copies in indexes are transitional unless a specific index implementation
requires them for performance. The preferred shape is searchable text plus a
canonical object id.

## Remaining Open Questions

- Which secondary-index `payload_json` copies are required for acceptable query
  performance, and which can be removed immediately?
