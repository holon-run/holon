# Prompt Benchmark Decisions

This document records the main tradeoffs made while implementing the first
benchmark wave and the first prompt-architecture refactor.

## 1. Benchmark Harness Lives In Node

Decision:

- implement the benchmark harness in `Node.js`

Why:

- `@anthropic-ai/claude-agent-sdk` is Node-first
- this avoids wrapping the SDK through an extra process layer for every call
- it keeps `HolonRunner` and `ClaudeSdkRunner` under one script contract

Consequence:

- `benchmark/package.json` is now part of the repo
- raw benchmark execution depends on `npm install` inside `benchmark/`

## 2. Use Fixed Local Fixtures, Not The Live Holon Repo

Decision:

- benchmark against committed fixtures under `benchmark/fixtures/`

Why:

- prompt work changes the Holon repo itself
- comparing against the live repo would confound “prompt changed” with
  “workspace snapshot changed”
- fixed fixtures make baseline and candidate runs comparable

Consequence:

- the first wave uses two small offline fixtures:
  - `analysis-runtime`
  - `greeting-bug`

## 3. Restrict Claude SDK Tools To Holon-Parity Surface

Decision:

- do not let the Claude SDK runner use the full default Claude Code tool set

Why:

- the point is to compare Holon’s current runtime against a Claude-style agent,
  not to give the SDK runner capabilities Holon does not have

Current tool restriction:

- read-only tasks:
  - `Read`, `Glob`, `Grep`
- coding tasks:
  - `Read`, `ApplyPatch`, `Glob`, `Grep`, `Bash`

## 4. Prompt Modes Flow Through Message Metadata

Decision:

- use `message.metadata.prompt_mode` as the lightweight selector for prompt mode

Why:

- no new message envelope type was necessary
- benchmark tasks can request a mode without introducing task-specific prompt
  branches
- runtime callers remain loosely coupled to prompt internals

Consequence:

- analysis and coding benchmarks can use the same enqueue path while selecting
  different prompt topologies

## 5. Split Stable Prompt Sections From Dynamic Context

Decision:

- move prompt assembly into `src/prompt.rs`
- keep `src/context.rs` focused on dynamic attachments only

Why:

- this follows the Claude Code style at a smaller scale
- it makes prompt changes inspectable and benchmarkable
- it prevents stable rules from being buried inside volatile session text

Consequence:

- `EffectivePrompt` now carries:
  - `system_sections`
  - `context_sections`
  - rendered system prompt
  - rendered context attachment

## 6. First Pass Tool Guidance Stays Small

Decision:

- only add prompt guidance for:
  - `Sleep`
  - `CreateTask`
  - `ExecCommand`
  - `EditFile`

Why:

- these tools materially affect convergence and output quality
- broader tool prompt coverage can wait until a larger benchmark corpus exists

## 7. Prompt-V1 Regression: Sleep Without A User Summary

Observed problem:

- after the first prompt refactor (`prompt-v1`), Holon still solved the coding
  task but sometimes ended with `Completed.` as the final brief

Root cause:

- the refactored prompt clarified when to call `Sleep`, but did not strongly
  require a final user-facing summary before sleeping

Decision:

- add a finishing contract to both the global reporting section and the
  `Sleep` tool section

Result:

- `prompt-v2` restored a proper final coding summary without adding any
  task-specific instructions

## 8. Keep The Architecture, Even If Raw Efficiency Does Not Improve Everywhere

Decision:

- keep the prompt-section architecture even though analysis-mode tool usage went
  up relative to the baseline

Why:

- inspectability improved materially
- prompt variants are now benchmarkable
- the `prompt-v1` regression was easy to diagnose and fix precisely because the
  prompt is now structured

Conclusion:

- architecture quality improved clearly
- raw benchmark efficiency improved only in selected cases
- future prompt work should target:
  - analysis-mode tool selection efficiency
  - broader benchmark coverage
  - better convergence heuristics
