# Objective, Delta, And Acceptance Boundary

Date: 2026-04-08

Related issue:

- `holon-run/holon#45` `RFC: objective, delta, and acceptance boundary model`

Related RFCs:

- [docs/result-closure-contract.md](result-closure-contract.md)
- [docs/continuation-trigger-contract.md](continuation-trigger-contract.md)

This RFC defines the minimum runtime model for preserving scope across turns,
follow-ups, delegation, and resumption.

The core question is:

- how should `Holon` remember what the current work actually is, what changed,
  and what currently counts as "done"?

## Problem

`Holon` already has queueing, waiting, closure, and continuation semantics.
But it still lacks a stable runtime-visible representation for current scope.

Today, the current objective mostly lives in:

- the latest operator prompt
- recent transcript messages
- prompt wording
- the model's own temporary understanding

This creates several problems:

- follow-up handling drifts because the runtime does not hold the current
  objective explicitly
- it is hard to tell whether a new input continues, narrows, replaces, or
  appends work
- task and child-agent handoff can return useful results without a clean scope
  merge rule
- compaction and resume risk preserving text while losing the actual current
  objective

`Holon` needs one explicit scope model.

## Goal

This RFC answers four questions:

1. What is the current objective?
2. What changed relative to the prior objective?
3. What currently counts as acceptable completion?
4. Who is allowed to define or update those things?

## Non-Goals

This RFC does not define:

- final prompt wording
- a fully automatic semantic parser for arbitrary operator text
- the complete delegation lifecycle
- approval UX
- a final summarization or compaction algorithm

Those may depend on this model, but they should not define it.

## Core Judgments

### 1. The runtime should hold the current objective record

The current objective should not live only in prompt text or in the model's
private understanding.

`Holon` should maintain a runtime-visible objective record.

That record becomes:

- inspectable
- auditable
- resumable
- safe to hand off across turns and agents

### 2. The agent may propose scope updates, but it should not be the only source of truth

The model often has the best short-term understanding of what it is currently
trying to do. That is useful, but it should not become the only authority.

So:

- the runtime owns the current objective record
- the agent may propose updates to it
- operator and system-originated inputs remain stronger signals than the
  agent's own summary

This avoids both extremes:

- pure runtime guessing from text
- pure agent self-report as truth

### 3. Objective and delta should be modeled separately

The current objective answers:

- what are we trying to do now?

The delta answers:

- how does the newest input change that objective?

Those are different questions and should stay separate.

### 4. Acceptance boundary should be explicit

An objective alone is not enough.

`Holon` also needs to represent the current acceptance boundary:

- what counts as done
- what is intentionally out of scope
- what would count as scope expansion

Without that, follow-ups and delegated results are too easy to mismerge.

## Model

The minimum model should include three records:

- `objective`
- `delta`
- `acceptance_boundary`

## Objective

The objective record should answer:

- what the agent is currently trying to achieve

Minimum fields should be:

- `objective_text`
- `source`
- `updated_at_turn`
- `related_message_id` or equivalent provenance

Recommended source values:

- `operator_input`
- `task_handoff`
- `agent_proposed`
- `runtime_derived`

Meaning:

- `operator_input`
  - objective set directly from operator-originated input
- `task_handoff`
  - objective set or refined by delegated-work rejoin
- `agent_proposed`
  - objective update proposed by the agent and accepted by runtime policy
- `runtime_derived`
  - minimal fallback objective synthesized by runtime when no stronger signal
    exists

## Delta

The delta record should answer:

- how the newest input changes the current objective

The first stable delta set should be:

- `continue`
- `narrow`
- `replace`
- `append`

Meaning:

- `continue`
  - proceed with the same objective
- `narrow`
  - keep the same objective lineage but reduce the active scope
- `replace`
  - discard the current objective as the active objective and switch to a new
    one
- `append`
  - keep the current objective and add another objective or sub-goal alongside
    it

`append` should be used conservatively. Most follow-ups should likely become
`continue`, `narrow`, or `replace`.

## Acceptance Boundary

The acceptance boundary should answer:

- what currently counts as acceptable completion for the active objective

Minimum fields should be:

- `done_when`
- `out_of_scope`
- `notes`

This does not require a rigid schema in phase 1. A compact textual structure is
enough as long as it is explicit and runtime-visible.

Examples:

- `done_when`
  - "fix the failing review comment and keep behavior unchanged elsewhere"
- `out_of_scope`
  - "do not redesign the workflow"

## Sources Of Truth

The runtime should merge three signal tiers.

### Tier 1: operator or system-originated scope

Strongest signals:

- initial operator prompt
- later operator follow-up
- explicit task or child-agent handoff metadata

These should define the baseline objective and delta.

### Tier 2: agent proposal

The agent may propose:

- current objective restatement
- narrowed acceptance boundary
- explicit statement of what is out of scope

This is useful, but should not automatically override stronger operator input.

### Tier 3: runtime derivation

If no explicit objective record is available, runtime may synthesize a minimum
fallback objective from:

- latest strong operator input
- current handoff payload
- current closure / continuation state

This is a safety fallback, not the preferred source.

## Agent Proposal

The model should be allowed to propose objective updates, but phase 1 should
keep this lightweight.

The important rule is:

- agent proposal is a proposal, not automatic truth

That means `Holon` may later support an explicit objective-update primitive,
but phase 1 does not need to depend on it.

Phase 1 can start with:

- runtime-held objective record
- operator/system updates
- optional agent-proposed summary fields accepted by simple rules

## Follow-Up Classification

When a new input arrives, the runtime should classify it relative to the
current objective:

- `continue`
- `narrow`
- `replace`
- `append`

The classification should be based primarily on:

- origin of the new input
- whether the input clearly supersedes the old objective
- whether it clearly reduces scope
- whether it introduces additional concurrent work

Phase 1 does not need perfect semantic classification. A conservative first
pass is enough if the result is explicit and inspectable.

## Delegation And Rejoin

Task and child-agent results should not be treated as free-floating text.

They should rejoin through the scope model.

That means delegated work should be able to carry at least:

- objective lineage
- acceptance-boundary notes
- result summary

When delegated work returns, the runtime should decide whether the result:

- completes the current objective
- narrows the remaining work
- appends follow-up work
- or merely contributes evidence without changing active scope

## Compaction And Resume

The objective record should survive compaction and resume as a first-class
state record.

That is the main reason this model should not remain prompt-only.

After compaction, `Holon` should still be able to answer:

- what is the current objective?
- what most recently changed it?
- what currently counts as done?

## Invariants

1. The runtime should always be able to expose one current objective record for
   the active execution context.
2. New input should be classified relative to the current objective, not only
   appended to transcript history.
3. Agent proposals may refine objective understanding, but should not silently
   override stronger operator intent.
4. Acceptance boundary should be explicit enough to distinguish completion from
   scope expansion.
5. Objective state should survive resume and compaction.

## Phase 1 Direction

Phase 1 should aim for a minimal runtime contract:

- add a runtime-visible objective record
- add a simple delta classification
- add a compact acceptance-boundary record
- store and expose them through summary and state surfaces

Phase 1 does not need:

- perfect semantic inference
- automatic objective rewriting on every turn
- complex multi-objective planning

## Decision

`Holon` should treat objective, delta, and acceptance boundary as explicit
runtime state. The runtime owns the current record, while agents may propose
updates. New inputs should be classified as `continue`, `narrow`, `replace`, or
`append`, and completion should be judged against an explicit acceptance
boundary rather than only prompt-local understanding.
