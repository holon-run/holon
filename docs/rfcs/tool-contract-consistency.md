---
title: RFC: Tool Contract Consistency
date: 2026-04-21
status: draft
---

# RFC: Tool Contract Consistency

## Summary

This RFC proposes a tightening pass for Holon's tool contracts so the public
surface becomes more consistent across:

- schema shape
- execution validation
- return envelopes
- prompt guidance
- naming discipline

The goal is not to add more tools. The goal is to make the existing tool
surface feel like one coherent system.

## Problem

Holon already has a substantial public tool surface, but the contract quality
is uneven.

Today the surface still mixes:

- typed schemas and manual value parsing
- structured JSON returns and plain-text receipts
- variant-shaped runtime semantics and flat input objects
- mature prompt guidance and weakly-documented tools
- public fields that the runtime ignores

This makes the surface harder for both the model and future maintainers:

- the model sees inconsistent result shapes
- tool chaining becomes less predictable
- prompt guidance has to compensate for contract drift
- new tools risk inheriting whichever style was most convenient locally

## Goals

- make tool contracts more uniform across the public surface
- reduce schema/runtime drift
- make return values easier to chain programmatically
- ensure tool prompt guidance reflects the real contract
- provide a stable style baseline for future tool additions

## Non-goals

- do not force every tool to use identical payloads
- do not remove useful Holon-specific semantics in the name of uniformity
- do not require a single giant migration PR

## Proposed Consistency Rules

## 1. Naming And Schema Conventions

Holon should adopt explicit naming and schema conventions for built-in tools.

### Tool names

Holon-native built-in tool names should default to:

- PascalCase
- no namespace prefix

Examples of the intended direction:

- `CreateTask`
- `TaskList`
- `CreateExternalTrigger`
- `UseWorkspace`
- `ExecCommand`
- `UpdateWorkItem`
- `UpdateWorkPlan`

The reason is not style purity by itself. The reason is contract clarity.

If a built-in tool name stays close to an external system's canonical tool id,
the runtime should also stay close to that external contract. Otherwise Holon
inherits misleading model priors.

This means Holon should not preserve `exec_command` as a long-term exception
unless it deliberately realigns the contract with Codex-style semantics. If the
Holon command plane is Holon-native, the name should also become Holon-native.

### Namespace policy

Holon-native built-ins should not use namespace prefixes such as:

- `task.list`
- `workspace.enter`
- `command.exec`

Namespace prefixes should be reserved for:

- external dynamic tools
- MCP-imported tools
- future multi-provider surfaces where source disambiguation matters

### Parameter fields

Tool parameter fields should use:

- `snake_case`

Examples:

- `task_id`
- `delivery_mode`
- `yield_time_ms`
- `max_output_tokens`

### Enum values

Enum values and discriminators should use:

- `snake_case`

Examples:

- `sleep_job`
- `child_agent_task`
- `wake_only`

### Field suffix conventions

Holon should use stable suffix conventions for common parameter families:

- ids: `*_id`
- durations: `*_ms`
- token counts: `*_tokens`
- line counts: `*_lines`
- page counts: `*_pages`

Field names for paths should stay semantically distinct:

- `file_path` for one file
- `path` for a general search or traversal path
- `workdir` for command execution root
- `cwd` for entered workspace current directory

### Transitional note

Current built-in naming is mixed:

- PascalCase built-ins such as `CreateTask`
- snake_case built-ins such as `exec_command` and `update_work_item`

This RFC treats that mixed state as transitional rather than normative.

## 2. Strong Variant Boundaries

When a tool has meaningfully different variants, the public contract should
make that visible.

This matters especially for mixed-shape surfaces such as:

- `CreateTask`

If required fields differ by variant, the public schema should reflect that
rather than relying on execution-time folklore.

## 3. Typed Parse Before Semantic Validation

Holon should prefer:

1. typed schema
2. typed deserialize
3. semantic validation

over:

1. generic `Value`
2. ad-hoc field extraction
3. tool-specific validation scattered in execution code

This reduces drift between:

- provider schema
- runtime behavior
- tests

## 4. Stable Output Envelopes By Tool Family

Holon should use one shared model-visible outer envelope for built-in tool
results. Tool families should define the inner `result` payload, not invent a
different outer success/error shape.

At minimum, the following families should converge:

- command execution
- task inspection/control
- work-item mutation
- file mutation
- waiting/callback mutation

Plain human-readable summary text may still be included, but it should not be
the only machine-visible result for important tools.

The shared outer contract is defined in
`docs/rfcs/tool-result-envelope.md`.

## 5. No Dormant Public Fields

If the public schema exposes a field, the runtime should either:

- honor it
- explicitly reject it
- or remove it from the public contract

Ignored public fields create one of the worst kinds of contract drift because
the model believes it is using part of the tool while the runtime silently does
nothing with it.

## 6. Stable Tool Surface Per Agent

For a given agent, tool existence should be stable by default.

That means the model-facing tool catalog should be derived primarily from:

- agent profile
- runtime capability
- current execution boundary state such as active workspace attachment

Tool existence should not usually drift merely because the current message has a
different provenance or trust label.

In particular, Holon should avoid making the same long-lived agent see one tool
catalog for:

- operator prompts

and a different catalog for:

- timer
- callback
- webhook
- channel
- task rejoin

unless the runtime is intentionally switching the agent into a different
profile.

Message provenance and trust still matter, but they should primarily affect:

- instruction precedence
- authority interpretation
- prompt framing
- audit labeling
- admission and provenance framing

not whether a tool exists at all in the current turn.

## 7. Prompt Guidance Must Match Runtime Reality

Prompt guidance is part of the effective tool contract.

That means prompt guidance should:

- reflect actual runtime behavior
- explain tool-family boundaries
- avoid describing retired or hidden surfaces as normal paths

Prompt guidance should not be the only place where contract precision lives,
but it should be aligned with the true behavior.

## 8. Naming Should Follow Plane Ownership

Tools should be named according to the plane they belong to:

- command plane
- agent plane
- waiting plane
- work plane

This reduces future confusion where one name suggests a different layer than
the runtime behavior it actually triggers.

## Immediate Priority Areas

The highest-value early targets are:

- built-in naming convergence toward PascalCase
- `CreateTask` variant precision
- `ExecCommand` field drift cleanup
- structured command result envelopes
- more consistent task-control output envelopes
- alignment between prompt guidance and current public tool catalog
- removal of trust-driven tool-surface drift for long-lived agents

## Rollout Strategy

The recommended rollout is incremental:

1. define the consistency rules
2. apply them to the highest-traffic tools first
3. add focused contract tests for those tools
4. use the resulting style as the baseline for later migrations

This avoids a large all-at-once surface rewrite while still moving Holon
toward a coherent public contract.

## Summary

Holon's tool surface should be judged not only by what capabilities exist, but
by whether they form one coherent contract.

The next step is not "more tools first." The next step is stronger contract
discipline across:

- schema
- execution validation
- output shape
- prompt guidance
- naming
