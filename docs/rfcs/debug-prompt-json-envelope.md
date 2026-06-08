---
title: RFC: Debug Prompt JSON Envelope
date: 2026-06-08
status: draft
Handle: rfc-debug-prompt-json-envelope
---

# RFC: Debug Prompt JSON Envelope

## Summary

Holon's debug prompt JSON output should be a stable, machine-readable envelope
around the prompt sections that were considered and rendered for a provider
turn. It is not a serialization of the runtime `Context` object.

The envelope must explain:

1. what text entered the final prompt,
2. where each section came from,
3. why each section was included or omitted,
4. whether truncation, missing data, or other diagnostic conditions occurred.

## Problem

Debugging prompt assembly currently needs two kinds of evidence:

- the exact prompt text that the provider saw,
- structured facts about section origin, priority, budgets, omissions, and
  truncation.

Dumping the raw runtime context object is not an acceptable public debug
contract because it exposes internal implementation shape, changes whenever the
runtime data model changes, and may include details that are not part of the
debug surface.

Dumping only rendered prompt text is also insufficient because it forces humans
and tools to infer provenance and budget decisions from plain text.

## Contract

The debug prompt output is a JSON envelope with ordered prompt sections. Each
section contains both a stable structured debug payload and the rendered text
that contributed to the final prompt.

```json
{
  "version": 1,
  "agent_id": "holon-dev",
  "turn_id": "turn_abc123",
  "work_item_id": "work_abc123",
  "prompt_profile": "default",
  "budgets": {
    "total_tokens": 120000,
    "total_chars": 480000
  },
  "sections": [
    {
      "id": "recent_turns",
      "kind": "context",
      "title": "recent_turns",
      "source": [
        {
          "type": "TurnRecord",
          "ref": "turn:turn_abc123"
        }
      ],
      "inclusion": {
        "state": "included",
        "reason": "current_continuation_chain"
      },
      "priority": "high",
      "budget": {
        "max_tokens": 8000,
        "max_chars": 32000
      },
      "actual_size": {
        "tokens": 1200,
        "chars": 5400
      },
      "truncated": false,
      "omitted": false,
      "omission_reason": null,
      "structured_payload": {
        "turns": [
          {
            "turn_id": "turn_abc123",
            "trigger": "system_tick",
            "provenance": {
              "origin": "runtime_system",
              "trust": "runtime_owned"
            },
            "input_refs": [],
            "produced_brief_refs": [
              "brief:brief_abc123"
            ]
          }
        ]
      },
      "rendered_text": "Recent turns:\n- Turn turn_abc123: ..."
    }
  ],
  "warnings": [
    {
      "section_id": "recent_turns",
      "code": "missing_reference",
      "message": "brief ref brief:brief_missing was not available",
      "severity": "warning"
    }
  ]
}
```

All top-level fields are part of the debug contract unless marked optional
below.

| Field | Required | Meaning |
| --- | --- | --- |
| `version` | yes | Integer schema version for this envelope. |
| `agent_id` | yes | Agent whose prompt was rendered. |
| `turn_id` | yes | Runtime turn for this prompt render. |
| `work_item_id` | no | Focused WorkItem when one shaped the prompt. |
| `prompt_profile` | yes | Prompt/rendering profile or model-facing profile. |
| `budgets` | yes | Overall prompt budget known to the renderer. |
| `sections` | yes | Ordered section records in final prompt order. |
| `warnings` | yes | Envelope-level diagnostics; empty when none occurred. |

## Section Records

Each section record describes one prompt section before provider-specific
lowering. The record is stable debug data, not the internal Rust struct used to
assemble the prompt.

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stable section id, such as `system`, `workspace_guidance`, or `recent_turns`. |
| `kind` | yes | Broad section kind: `system`, `context`, `tool_guidance`, `memory`, `work_item`, `current_input`, or `diagnostic`. |
| `title` | yes | Human-readable title used in prompt dumps. |
| `source` | yes | Ordered source references used to build the section. |
| `inclusion` | yes | Inclusion state and reason. |
| `priority` | no | Runtime retention or inclusion priority when available. |
| `budget` | no | Section budget assigned before rendering. |
| `actual_size` | yes | Rendered section size after budgeting. |
| `truncated` | yes | Whether content was cut to fit a budget. |
| `omitted` | yes | Whether the section was considered but not rendered. |
| `omission_reason` | no | Reason when `omitted` is true. |
| `structured_payload` | yes | Stable debug payload for machine analysis. |
| `rendered_text` | yes | Exact text fragment rendered into the prompt for this section; empty when omitted. |

`source` entries should preserve provenance without exposing raw private
runtime objects. Use stable reference types such as:

- `TurnRecord`
- `Message`
- `Brief`
- `ToolExecution`
- `Memory`
- `WorkItem`
- `AgentGuidance`
- `WorkspaceGuidance`
- `ToolGuidance`
- `ExternalEvent`
- `RuntimePolicy`

`inclusion.state` should be one of:

- `included`
- `omitted`
- `truncated`
- `summarized`
- `redacted`

`inclusion.reason` should be a short stable reason, for example:

- `current_input`
- `current_continuation_chain`
- `focused_work_item`
- `operator_instruction`
- `active_tool_surface`
- `workspace_scope`
- `budget_exceeded`
- `missing_reference`
- `trust_boundary`

## Structured Payload Boundary

`structured_payload` is section-specific stable debug data. It should contain
the facts needed to analyze and diff that section, not the complete source
objects.

Examples:

- A `recent_turns` payload can list turn ids, trigger kind, provenance,
  referenced brief ids, referenced tool execution ids, and rendered summaries.
- A `work_item` payload can include the focused WorkItem id, objective,
  readiness, active wait summary, plan preview metadata, and todo snapshot.
- A `tool_guidance` payload can include tool names, guidance source paths, and
  activation reason.
- A `memory` payload can include memory source refs, match reasons, and whether
  a result was summarized or omitted.

The payload must not promise to mirror internal runtime structs. If the runtime
renames fields or changes storage, this debug envelope should remain stable
unless the external debug contract intentionally versions forward.

## Warning Records

Warnings report prompt assembly diagnostics that should not be silently hidden.

| Field | Required | Meaning |
| --- | --- | --- |
| `section_id` | no | Section associated with the warning, if any. |
| `code` | yes | Stable warning code. |
| `message` | yes | Human-readable diagnostic. |
| `severity` | yes | `info`, `warning`, or `error`. |

Expected warning codes include:

- `missing_reference`
- `section_omitted`
- `section_truncated`
- `payload_redacted`
- `budget_exceeded`
- `source_unavailable`
- `provider_lowering_changed_order`

## Provider Lowering Boundary

The envelope describes Holon's prompt sections before provider-specific request
lowering. Provider transports may later split, merge, cache-mark, or otherwise
lower these sections into a provider request. If a debug view needs to inspect
the provider request, it should expose that as a separate provider-transport
debug artifact linked from this envelope, not by changing section semantics.

## Compatibility Rules

- Consumers must ignore unknown fields.
- New optional fields may be added without changing `version`.
- Removing fields, changing required field meaning, or changing enum values
  requires a `version` bump.
- `rendered_text` may be empty only when `omitted` is true or a section was
  fully redacted.
- `warnings` must contain a diagnostic when a referenced source cannot be
  resolved and that missing source affects a section.

## Non-Goals

- Do not define the internal prompt assembly structs.
- Do not expose raw runtime `Context` as the debug JSON format.
- Do not replace rendered prompt dumps; `rendered_text` remains part of the
  contract.
- Do not define provider-specific HTTP request bodies.
- Do not make debug envelope data feed back into scheduling, retention, cache
  keys, or prompt assembly behavior.
