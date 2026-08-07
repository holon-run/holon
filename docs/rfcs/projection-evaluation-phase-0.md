---
title: RFC: Deterministic Projection Evaluation Phase 0
date: 2026-08-07
status: accepted
---

# Deterministic Projection Evaluation Phase 0

This RFC freezes the acceptance layer that must exist before Holon replaces the
legacy prompt projector with an owner-centered projector.

It complements:

- [Turn-Based Context Projection](./turn-based-context-projection.md)
- [Work Item Centered Agent Runtime](./work-item-centered-agent-runtime.md)
- [Continuation Anchor](./continuation-anchor.md)
- [Provenance, Admission, and Authority](./default-trust-auth-and-control.md)

## Scope

Phase 0 observes the current projector. It does not change provider input,
activation admission, durable ownership, or scheduling.

The shared diagnostic contract is `ProjectionManifest`. Debug tooling,
fixtures, and benchmark comparisons must consume that manifest rather than
reconstructing semantics from rendered Markdown.

## Manifest Contract

A manifest records:

- the legacy activation binding summary and its evaluation owner label
- projected sections in deterministic order
- full, compact, truncated, or omitted representation
- selected and omitted canonical evidence references
- omission or selection reason
- prompt budget and deterministic token estimates
- content hashes rather than duplicated prompt bodies
- deterministic invariant results

Serialization is schema-versioned, pretty-printed JSON with a trailing newline.
Stable inputs must produce byte-identical output.

`legacy_unbound` is an evaluation label for the absence of a durable WorkItem
binding. It is not a new persisted Conversation or AgentLifecycle owner.

## Canonical Evidence And Uniqueness

A durable fact has one canonical reference, such as `message:<id>`,
`turn:<id>`, `brief:<id>`, or `tool_execution:<id>`.

Within one manifest:

1. a selected canonical reference may appear only once
2. one section id has exactly one representation
3. omitted evidence remains visible as omitted evidence, not as a second
   selected projection
4. comparison uses canonical references and structured fields, never Markdown
   parsing

These rules detect the duplicate-projection failure mode that motivated Phase
0: adding a second dialogue path must not silently select the same Message,
AssistantRound, Brief, or tool result twice.

## Authority And Owner Boundary

Every evidence reference carries the binding label observed on its durable
record. The evaluator fails selected evidence whose label differs from the
activation label.

Hard authority failures include:

- a selected WorkItem fact from another WorkItem
- unbound discussion evidence selected into a WorkItem activation
- WorkItem evidence selected into an unbound discussion activation
- lifecycle, timer, external, or task evidence treated as operator instruction
- a candidate changing the activation binding summary

An LLM judge cannot override these failures.

## Required Deterministic Invariants

The required model-free checks are:

- prompt budget is respected
- current input is selected exactly once
- an identified direct predecessor is retained exactly once
- selected canonical evidence is unique
- selected evidence matches the activation owner label
- section representations are exclusive
- a smaller budget selects no evidence absent from the larger-budget result
- restart reconstruction is byte-equivalent

Mutation tests must prove each relevant invariant can fail.

## Corpus

The frozen corpus is under `benchmarks/projection-eval/corpus/`. It covers:

- short Chinese references from issue #2512
- 10, 20, and 40 Turn follow-up chains
- new-topic and return-to-old-topic boundaries
- WorkItem create, switch, return, wait, task-result, and recovery paths
- external, timer, and agent-lifecycle events interleaved with discussion
- 2k, 4k, and 16k budgets
- restart and compaction reconstruction

The corpus describes durable facts and expected hard assertions. It does not
encode an ideal future projector.

The frozen legacy baseline is an observation. A baseline case may fail a rubric
assertion; freezing that failure is useful because a candidate must not hide or
rewrite it.

## Benchmark Boundary

`benchmark/run.mjs projection-eval --suite <suite.json>` runs baseline and
candidate commands against identical case inputs, shared configuration, and
repetition counts.

The adapter records hard assertions plus diagnostic metrics when supplied:

- continuity
- false carry-over
- authority
- success
- verification
- tokens
- rounds
- tool calls
- duration

Baseline-only runs always complete successfully when their artifacts are
valid, even when the frozen baseline misses rubric assertions. Candidate
go/no-go requires every candidate case to pass and forbids assertion
regressions.

Model repetitions and pairwise LLM judgment may be attached as diagnostics.
They are not normal required CI gates and cannot override hard failures.

## Go / No-Go Contract

A candidate projector is eligible for implementation review only when:

1. all model-free tests pass
2. restart output is byte-equivalent
3. no authority or owner assertion regresses
4. no duplicate canonical reference is introduced
5. budget reduction is monotonic
6. the frozen corpus and thresholds are unchanged, except through a separately
   reviewed evaluation-contract change
7. production provider input remains unchanged until the candidate projector
   is explicitly enabled in a later phase

Phase 0 acceptance does not authorize owner migration or provider-native
cross-Turn conversation lowering.
