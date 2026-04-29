# Session Handoff

Date: 2026-04-06
Context: handoff for a new agent session after a long `pre-public runtime` dogfooding and benchmark-planning conversation

## Current state

This round of supervised `pre-public runtime` dogfooding is complete.

Five real tasks were finished and merged:

- `#32`
- `#15`
- `#19`
- `#31`
- `#33`

The most important system result is that `pre-public runtime` completed a real supervised
development loop with `AgentInbox` in the middle:

- enter managed worktree
- implement a real issue
- open a PR
- subscribe to GitHub review/comment events through `AgentInbox`
- sleep
- wake on real review activity
- read inbox items through `agentinbox inbox list/read`
- repair the PR
- push again

This flow is now validated enough to use as the baseline for the next round of
work.

## Important conclusions from this session

### 1. The review wake loop works

`AgentInbox -> pre-public runtime -> agentinbox CLI -> repair PR` is working for real GitHub
review/comment events.

This is the biggest practical outcome of the whole session.

### 2. CI events are still missing from the real GitHub integration

The current `AgentInbox` GitHub source supports real review/comment style
events, but not CI-oriented events such as `check_run` / `check_suite`.

This was recorded in:

- `holon-run/agentinbox#2`

So:

- real review-driven continuation is working
- real CI-driven continuation is not yet available

### 3. Prompt fidelity mattered more than expected

One major bug found and fixed in this session:

- long operator prompts were being truncated in prompt assembly because
  `current_input` went through a preview path

That was fixed in pre-public runtime and materially improved downstream execution
reliability.

### 4. `TaskOutput` was a necessary product primitive

The addition of `TaskOutput` significantly improved task coordination and
verification handling.

Even after that, verification is still the weakest phase, but the product is in
a meaningfully better state than before.

## New documents added during this session

### pre-public runtime internal notes

- [Dogfooding retrospective (2026-04)](./dogfooding-retrospective-2026-04.md)
- [Dogfooding action items (2026-04)](./dogfooding-action-items-2026-04.md)
- [Benchmark framework: manifest and naming (2026-04)](./benchmark-framework-manifest-and-naming-2026-04.md)

### Existing supervision log

- [Dogfooding supervision log (2026-04-05)](./dogfooding-supervision-log-2026-04-05.md)

This remains the primary source of detailed operational findings.

## Issues created during this session

### In `runtime-incubation`

- `#40` Feature: add a one-shot local execution mode
  - purpose: add something like `runtime-incubation run` so benchmark tasks can execute
    directly in-process without requiring `runtime-incubation serve`

### In `agentinbox`

- `#2` GitHub source should support CI-oriented events, not just repo activity feed

## Benchmark planning status

We started designing a benchmark framework for:

- `pre-public runtime + OpenAI model` vs `Codex + OpenAI model`
- `pre-public runtime + Claude model` vs `Claude Agent SDK + Claude model`

The current position is:

- main benchmark should use real repo tasks, not synthetic mock tasks
- we first documented:
  - task manifest design
  - branch / worktree / PR / label naming
- this is intentionally incomplete

The benchmark design doc currently covers:

- benchmark task manifest schema
- naming conventions for branch/worktree/PR/labels/artifacts

See:

- [Benchmark framework: manifest and naming (2026-04)](./benchmark-framework-manifest-and-naming-2026-04.md)

## Recommended next topics for the next session

Pick one of these, not all at once:

1. Continue benchmark framework design
   - result collection schema
   - evaluation metrics
   - standardized review injection

2. Implement `runtime-incubation` issue `#40`
   - add one-shot local execution mode

3. Turn dogfooding action items into concrete `runtime-incubation` issues
   - tighten verification loop
   - harden constrained repair mode
   - enforce managed worktree for supervised coding flows

4. Expand `AgentInbox` GitHub integration planning
   - especially how to handle CI-oriented events after `agentinbox#2`

## Environment and operational notes

This session used both formal local services and local project worktrees.

The exact live process state should be re-checked in the new session rather than
blindly assumed.

If the next agent wants to resume local integration work, re-confirm:

- `pre-public runtime` service status
- `AgentInbox` service status
- default model configuration
- current GitHub source / subscription state in `AgentInbox`

Do not assume the long-lived sleeping dogfooding agents are still the right
starting point for the next round.

Use fresh agents for new substantial tasks.

## Working rule for the next agent

Do not continue the previous five-task dogfooding loop.

That loop is done.

The next session should treat this as a fresh phase:

- use the retrospective and action-items docs as the new baseline
- create new tasks or benchmark steps explicitly
- avoid inheriting old sleeping agents unless there is a very specific reason
