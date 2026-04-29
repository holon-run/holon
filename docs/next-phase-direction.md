# Holon Next Phase Direction

This document records the recommended next major phase for `Holon`.

It is based on the current codebase state, recent implementation history, and
the current architecture documents.

It should now be read together with [architecture-overview.md](architecture-overview.md):

- `architecture-overview.md` maps the current runtime shape to canonical RFCs
- this document defines the recommended execution focus for the next phase

## Current Stage Judgment

`Holon` is no longer in the "can this runtime work at all" stage.

It has already crossed into a new phase:

- the core runtime semantics are established
- the coding loop is real
- worktree-isolated subagent workflows are real
- the main risk is no longer missing capability
- the main risk is structural sprawl

In short:

- phase 1 proved that `Holon` can be a long-lived coding-capable runtime
- phase 2 should make that runtime easier to grow without losing clarity

## What Is Already Working

The current repository already has a meaningful first end-to-end runtime:

- queue-centered session model
- explicit `origin` / `trust` / `priority`
- wake / sleep / timer / ingress behavior
- `brief` separated from internal execution
- tool-use / tool-result coding loop
- file and shell tools
- bounded `child_agent_task`
- worktree-isolated task execution
- multi-session host
- regression, HTTP, live-provider, and worktree tests

This means the project does not primarily need "more features" next.

It needs a cleaner shape for the features it already has.

## Core Judgment

The next phase should keep Claude-like runtime semantics while moving toward
Codex-like structural boundaries.

In short:

- Claude remains the semantic teacher
- Codex becomes the structural teacher

This should be done incrementally.

The next phase should not be a large rewrite and should not be framed as
"becoming Codex".

It should also remain benchmark-guarded.

Structural refactoring should happen under stable behavioral expectations, not
as a free-form architectural rewrite.

## Recommended Theme For The Next Phase

The next phase should be:

- structural consolidation under stable semantics

That means:

- preserve the current queue / wake / sleep / brief / provenance model
- preserve the current coding-oriented runtime behavior
- improve module boundaries so capability can continue to grow safely

This theme depends on one important operating rule:

- semantic changes and structural changes should be separated whenever possible

That makes it easier to use benchmark and regression evidence as a safety rail
instead of mixing too many variables into one change.

## Priority 1: Split Runtime Boundaries

This is the highest-priority structural problem in the repository.

`src/runtime.rs` is now too large and carries too many responsibilities.

The next phase should extract clearer runtime slices such as:

- session lifecycle
- turn execution
- task orchestration
- delivery / brief synthesis
- sleep / wake / timers
- worktree coordination

Goal:

- make the runtime easier to reason about
- reduce the chance that every new feature expands one central file
- preserve current behavior while improving internal separation
- keep benchmark behavior and regression tests as the change budget for the
  refactor

## Priority 2: Build A Real Prompt Assembly System

The next step is not "write a better prompt".

The next step is:

- build a better prompt system

Current prompt and context behavior is already useful, but it is not yet clean
enough for the next stage.

The next phase should separate:

- stable instructions
- mode-specific guidance
- tool-specific guidance
- dynamic session attachments

This matters because `Holon` now has enough runtime shapes that one large prompt
string will become harder to benchmark and maintain.

Goal:

- prompt inspectability
- clearer mode behavior
- less accidental coupling between session state and stable instructions
- less confusion about whether a limitation is prompt-level or structural

## Priority 3: Promote Worktree Flow Into A First-Class Workflow

Worktree support is no longer experimental in spirit.

It is already becoming one of the most distinctive parts of `Holon`.

The next phase should treat worktree-based parallel coding as a first-class
workflow rather than only a set of tools.

That means improving:

- coordinator behavior
- candidate result summaries
- review / keep / task-owned cleanup flow
- task metadata clarity

But it should still avoid:

- automatic merge
- hidden integration decisions

Goal:

- make worktree subagent execution a reliable reviewable workflow
- not just a low-level capability

## Change Discipline For This Phase

The next phase should follow a stricter change discipline than the earlier
buildout phase.

### 1. Do Not Wait Too Long To Clean Structure

If structural cleanup is delayed until after more prompt, worktree, and tool
growth, the later refactor will become more expensive and more confusing.

### 2. Do Not Launch A Broad Fusion Rewrite

This phase should not become a large Claude/Codex fusion rewrite.

That would mix:

- behavior changes
- structural changes
- benchmark shifts

and make the project harder to reason about.

### 3. Use Benchmarks And Regression Tests As Safety Rails

The project now has enough measurement and regression infrastructure that
changes should increasingly be judged by:

- benchmark behavior
- prompt inspectability
- regression stability
- clearer module boundaries

not only by whether a refactor feels architecturally cleaner on paper.

## What Should Not Be The Main Focus Yet

The next phase should not primarily focus on:

- UI-first work
- plugin ecosystems
- many new ingress transports
- automatic merge/cherry-pick workflows
- broad new feature expansion inside the current large runtime files
- product-shaped Claude/Codex fusion work

These may become useful later, but they are not the best next bottleneck to
attack now.

## Recommended Order

The recommended order for the next phase is:

1. runtime boundary extraction
2. prompt assembly refactor
3. worktree workflow hardening

This order matters.

If worktree and prompt behavior continue to grow before runtime boundaries are
cleaned up, the later refactor will become more expensive.

This also means:

1. separate structural cleanup from broad semantic rewrites
2. prefer benchmark-checked extractions over large architectural jumps
3. delay tool-surface expansion until the current runtime shape is easier to
   reason about

## Closing Summary

If the previous phase was about proving:

- `Holon` can be a long-lived coding-capable runtime

then the next phase should be about proving:

- `Holon` can keep growing without collapsing into one large runtime module

Or, more compactly:

- Claude helps define what `Holon` should do
- Codex helps define how `Holon` should be structured
