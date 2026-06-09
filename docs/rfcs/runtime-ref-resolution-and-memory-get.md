---
title: RFC: Runtime Ref Resolution and MemoryGet
date: 2026-06-10
status: draft
Handle: rfc-runtime-ref-resolution-and-memory-get
---

# RFC: Runtime Ref Resolution and MemoryGet

## Summary

Holon prompt projection should expose compact retrieval anchors, and agents
should be able to dereference those anchors through one stable runtime contract.

This RFC defines a typed runtime ref model for `MemoryGet`:

- `MemorySearch` discovers candidate refs through an index.
- `MemoryGet` dereferences known refs through their authoritative runtime
  stores wherever practical.
- Prompt-projected refs are stable anchors, while inline previews are bounded
  hints.

The goal is to make context compaction recoverable without turning the search
index into a source of truth or embedding large runtime records in every prompt.

## Related documents

- [Agent and Workspace Memory](./agent-and-workspace-memory.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Turn-Based Context Projection](./turn-based-context-projection.md)
- [Runtime Ledger Files and Relations](./runtime-ledger-files-and-relations.md)
- [Tool Surface Layering](./tool-surface-layering.md)
- [Tool Result Envelope](./tool-result-envelope.md)

## Problem

Holon is moving prompt context toward smaller, ref-oriented surfaces:

- `recent_turns` should show continuity and retrieval anchors, not replay every
  tool detail.
- `episode` should become an anchor surface rather than a transcript summary.
- future WorkItem refs should point to files, tools, issues, tasks, or evidence
  that may need to be reopened.

That design only works if an agent can later fetch exact content behind a known
anchor. Otherwise prompt compaction only moves information out of sight:

- a prompt can mention `brief_ref`, `cmd_ref`, `stdout_ref`, or `task` evidence,
  but lookup rules remain partly ad hoc;
- `MemorySearch` can become the accidental source of truth because `MemoryGet`
  only retrieves what the index already knows how to return;
- each runtime object can grow its own ref syntax and validation path;
- old previews can be mistaken for the authoritative content they summarize.

Holon needs one explicit boundary:

```text
Search discovers refs.
Get dereferences refs.
Stores own the original content.
Prompt previews are only hints.
```

## Goals

- Define a canonical typed ref contract for model-visible runtime anchors.
- Keep `MemorySearch` as an index-backed discovery tool, not a canonical store.
- Make `MemoryGet` resolve known refs directly against original stores or
  evidence repositories where practical.
- Preserve trust, provenance, agent, and workspace visibility boundaries.
- Keep prompt projection compact while ensuring exposed refs are fetchable.
- Make missing, invalid, unsupported, and unauthorized refs fail with clear,
  bounded tool errors.
- Keep Ack lifecycle receipts outside the semantic `brief:` namespace.

## Non-Goals

- Do not make every internal runtime event dereferenceable.
- Do not introduce LLM summarization into ref resolution.
- Do not make the search index authoritative for runtime evidence.
- Do not define the complete future `WorkItem.work_refs` schema.
- Do not define a global cross-agent search capability.
- Do not expose secrets, credentials, hidden prompt internals, or raw provider
  requests through `MemoryGet`.

## Terms

### Runtime Ref

A runtime ref is an opaque, typed handle that identifies retrievable Holon data.
It is copied from prompt projection, `MemorySearch`, or another runtime-owned
tool response.

Refs are not file paths, URLs, search queries, or natural-language labels.

### Resolver

A resolver maps one supported ref namespace to an authoritative source. For
example, a `brief:` resolver reads semantic brief evidence, and a
`tool_execution:` resolver reads tool execution evidence.

### Preview

A preview is bounded model-facing text rendered near a ref. It helps the agent
decide whether to dereference the ref, but it is not the source of truth.

## Ref Syntax

Refs should use a short namespace followed by opaque identifier segments:

```text
<namespace>:<id>
<namespace>:<id>:<selector>
<namespace>:<id>:<subresource>:<sub_id>:<selector>
```

General rules:

- namespace names are lower snake case;
- ids are opaque and should not be parsed for semantics outside the owning
  resolver;
- refs must be single tokens without whitespace;
- refs should avoid embedding local absolute paths or capability URLs;
- refs may include selectors when one runtime object has multiple retrievable
  subresources.

## Initial Namespaces

| Namespace | Example | Source of truth | Notes |
| --- | --- | --- | --- |
| `agent_memory:` | `agent_memory:self` | curated agent memory file | Agent-scoped durable memory, not runtime evidence. |
| `workspace_profile:` | `workspace_profile:ws-holon` | workspace profile store | Workspace-scoped profile content. |
| `brief:` | `brief:brief_123` | semantic brief evidence | Only semantic user-facing delivery such as result or failure. Ordinary Ack is not a brief ref target. |
| `tool_execution:` | `tool_execution:tool_123:stdout` | tool execution evidence | Supports selected command input/output subresources. |
| `task:` | `task:task_123` | task lifecycle store/evidence | Returns bounded task state and terminal result evidence when available. |
| `work_item:` | `work_item:work_123` | WorkItem state store | Returns current/latest WorkItem state and stable plan/result refs where available. |
| `episode:` | `episode:ep_123` | context episode store | Returns archived episode record content and source refs. |

The initial `tool_execution:` selectors are:

```text
tool_execution:<id>:cmd
tool_execution:<id>:stdout
tool_execution:<id>:stderr
tool_execution:<id>:output
tool_execution:<id>:batch_item:<index>:cmd
tool_execution:<id>:batch_item:<index>:stdout
tool_execution:<id>:batch_item:<index>:stderr
tool_execution:<id>:batch_item:<index>:output
```

`<index>` is one-based and must identify a batch item in the stored tool
execution record.

## Brief Boundary

After Ack normalization, `brief:` means semantic user-facing delivery.

Allowed `brief:` targets include:

- result briefs;
- failure briefs;
- WorkItem completion reports when represented as semantic delivery;
- future explicit blocker or wait reports only when they are user-facing
  outcomes rather than lifecycle receipts.

Ordinary admission acknowledgements such as `Queued work: ...` are lifecycle
or control-plane evidence. They should be stored and inspected through their
own event/evidence surface if Holon later exposes one, not through `brief:`.

Legacy `BriefKind::Ack` rows may remain readable for compatibility, but new
prompt projection should not mint `brief:` refs for Ack rows, and ref resolution
should not treat Ack as the semantic brief model.

## Search and Get Contract

### MemorySearch

`MemorySearch` is discovery:

- it reads an index projection;
- it ranks and filters candidate refs;
- it returns source refs, snippets, metadata, and provenance;
- it may omit valid refs that are not indexed or not relevant to the query.

Search results are not canonical object bodies. An indexed snippet must not be
treated as exact historical content.

### MemoryGet

`MemoryGet` is dereference:

- it validates the ref syntax and namespace;
- it checks the caller-visible agent/workspace/trust boundary;
- it resolves the ref through the namespace's authoritative store when
  practical;
- it returns exact bounded content plus metadata describing truncation and the
  resolved source.

When a known ref is available from prompt projection, `MemoryGet` should not
require `MemorySearch` to be called first. Search can discover refs, but it
should not be required to fetch a known prompt ref.

### Index Fallback

The search index may remain a compatibility fallback while a namespace is being
migrated, but fallback behavior must be explicit:

- source-of-truth resolver first;
- index fallback only when no direct resolver exists yet;
- clear tests for namespaces that promise direct resolution;
- no write path should treat the index as canonical state.

## Visibility and Trust

Every resolver must preserve the runtime boundary of the underlying object.

Minimum checks:

- the requesting agent can see the object;
- workspace-scoped data respects current workspace visibility unless the ref is
  explicitly global or agent-scoped;
- trust/provenance metadata is retained in the response where useful;
- hidden provider internals, credentials, and capability secrets are not
  exposed just because they appear in an evidence record.

`MemoryGet` may return a clear unauthorized error instead of pretending a valid
but hidden ref is missing when that distinction is safe. If revealing existence
would leak sensitive information, it may return a bounded not-found style error
with no secret details.

## Error Classes

The tool contract should distinguish these cases:

- `invalid_ref`: the ref is malformed or uses an invalid selector.
- `unsupported_ref_namespace`: the namespace is syntactically valid but not
  exposed through `MemoryGet`.
- `memory_source_not_found`: the ref is valid and supported, but no visible
  source exists.
- `memory_source_unauthorized`: the source exists but is outside the caller's
  visibility boundary, when safe to disclose.
- `memory_source_unavailable`: the source exists but cannot currently be read.

All errors should be bounded and include recovery guidance. They must not dump
large source bodies, secrets, or raw database errors.

## Prompt Projection Requirements

Any prompt section that exposes a runtime ref should follow these rules:

- expose the exact ref string the resolver accepts;
- include only a bounded preview near the ref;
- make truncation explicit when preview text is incomplete;
- avoid generating refs for unsupported or intentionally private objects;
- keep refs stable across compaction and prompt budget changes;
- prefer refs over embedding large stdout, stderr, tool outputs, episode text,
  or older result bodies.

For adjacent conversation continuity, prompt projection may inline recent
semantic delivery text. That does not weaken the ref contract: the same
semantic output should remain fetchable through `brief:` when a `brief_ref` is
shown.

## Resolver Shape

Implementation should converge on a shared parser and resolver boundary instead
of duplicating validation in each tool.

Suggested shape:

```text
RuntimeRef
  namespace
  id
  selector

RuntimeRefResolver
  validate(ref)
  resolve(ref, caller_context, max_chars) -> MemoryGetResult
```

The parser owns syntax. Resolvers own namespace-specific semantics and source
lookup.

The caller context should include at least:

- agent id;
- active workspace id or visible workspace set;
- authority class;
- any future host-level capability for broader retrieval.

## Rollout Plan

1. Add this RFC and align tool documentation with the Search/Get boundary.
2. Extract current `MemoryGet` source-ref validation into a shared runtime ref
   parser.
3. Add direct resolver coverage for existing prompt-visible refs:
   `brief:`, `tool_execution:`, `task:`, `work_item:`, and `episode:`.
4. Keep `MemorySearch` index-backed, but ensure search-returned refs can be
   passed directly to `MemoryGet`.
5. Update prompt projection code to use only refs accepted by the shared parser.
6. Add compatibility handling for legacy Ack briefs without minting new
   semantic `brief:` refs for Ack.
7. Use this contract when adding future WorkItem-owned `work_refs`.

## Test Expectations

Focused tests should cover:

- parser acceptance and rejection for every supported namespace;
- direct `MemoryGet` of known prompt refs without calling `MemorySearch`;
- `MemorySearch` returning refs that `MemoryGet` can dereference;
- `brief:` returning semantic result/failure content and not depending on Ack;
- tool execution stdout/stderr/output refs for command and batch executions;
- task, WorkItem, and episode refs resolving from their source stores;
- invalid, unsupported, missing, unauthorized, and unavailable errors;
- prompt snapshots showing refs that are accepted by the parser.

## Open Questions

- Should Holon expose an `event:` or `audit_event:` namespace for selected
  lifecycle receipts such as acknowledgements, or should those remain
  operator/debug API concerns?
- Should `turn:` become a direct `MemoryGet` namespace, or should turns remain
  reachable only through episode, brief, task, and tool refs until turn
  projection stabilizes?
- How should future host-level or cross-agent retrieval be authorized without
  weakening default agent/workspace isolation?
- Should file refs ever be handled by `MemoryGet`, or should workspace files
  remain exclusively under execution/file tools?
