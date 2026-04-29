# Holon Prompt Architecture Roadmap

This document defines how `Holon` should evolve from a single runtime prompt
string into a prompt assembly system that can be tested, inspected, and
compared against external baselines.

The design reference is Claude Code’s organization style, but the goal is to
keep the implementation smaller and cleaner.

## Why This Needs To Change

Today, `Holon` has two prompt layers:

- the runtime system prompt in `src/runtime.rs`
- the context bundle rendered in `src/context.rs`

That is enough to run coding loops, but it has several weaknesses:

- the prompt is difficult to inspect
- behavior guidance and tool guidance are mixed together
- there is no mode-specific prompt topology
- prompt changes are hard to benchmark cleanly
- high-volatility state is mixed with stable instructions

The next step is not “write a better prompt”. The next step is to build a
better prompt system.

## Current Shape

Today the effective prompt is roughly:

1. `RuntimeHandle::system_prompt()`
2. `build_context(...)` output as the first user message
3. tool schemas from `ToolRegistry::tool_specs(...)`

That means:

- base identity lives in a single string
- working memory is rendered as plain text sections
- per-tool behavioral guidance is mostly absent
- prompt mode changes are implicit rather than modeled

## Design Goals

The new prompt architecture should achieve five things:

- separate stable instructions from volatile session state
- separate runtime-wide instructions from per-tool instructions
- support multiple prompt modes without giant `if` strings
- make effective prompts inspectable
- make prompt variants benchmarkable

## Proposed Prompt Layers

The effective prompt should be assembled from five layers.

### 1. Base Sections

These are stable instructions that should apply to most `Holon` runs.

Examples:

- runtime identity
- trust boundary basics
- brief/reporting contract
- sleep contract
- coding truthfulness and verification requirements

These sections should change rarely.

### 2. Mode Sections

These depend on the runtime mode.

Initial modes should be:

- `interactive_coding`
- `headless_daemon`
- `subagent`
- `analysis`

The point is not to support every future mode immediately. The point is to stop
forcing every run shape through one monolithic instruction string.

### 3. Tool Sections

Each tool should be allowed to contribute behavioral guidance.

Examples:

- `Sleep`: when to terminate, when not to idle-spin
- `CreateTask`: what counts as a bounded task, what should not be delegated
- `ExecCommand`: when to prefer shell, when to avoid it
- `EditFile`: when to patch in place versus rewrite

This should live alongside the tool implementation rather than in one central
prompt blob.

### 4. Dynamic Attachments

These are high-volatility per-session materials that should not be treated as
stable instructions.

Examples:

- current session summary
- recent messages
- recent briefs
- recent tool executions
- active task list
- current workspace root

This is conceptually similar to Claude Code’s dynamic prompt sections and
attachment-style reminders, but Holon can start with a simpler implementation.

### 5. Effective Prompt Builder

A single builder should own final assembly.

It should be responsible for:

- selecting the prompt mode
- ordering sections deterministically
- collecting tool guidance
- rendering dynamic attachments
- producing the final prompt artifact for debugging and benchmarking

## Proposed Types

The first implementation should add explicit prompt types.

Suggested starting shapes:

```rust
enum PromptMode {
    InteractiveCoding,
    HeadlessDaemon,
    Subagent,
    Analysis,
}

struct PromptSection {
    name: &'static str,
    content: String,
    stability: PromptStability,
}

enum PromptStability {
    Stable,
    AgentScoped,
    TurnScoped,
}

struct EffectivePrompt {
    mode: PromptMode,
    sections: Vec<PromptSection>,
    rendered_system_prompt: String,
    rendered_context_attachment: String,
}
```

The exact Rust shape can change, but the architecture should preserve these
concepts.

## Phase Plan

### Phase 1: Extract The Current Prompt

Goal:

- move the current inline runtime prompt out of `RuntimeHandle::system_prompt()`

Includes:

- create a prompt module
- define base section builders
- keep behavior identical to current runtime

Definition of done:

- no functional prompt change yet
- existing tests still pass
- the effective prompt can be dumped from one code path

### Phase 2: Split Stable Instructions From Session Context

Goal:

- stop mixing durable instructions with volatile context text

Includes:

- render stable system prompt separately
- render context bundle separately
- keep section ordering deterministic

Definition of done:

- prompt dumps clearly show system sections vs context sections
- benchmark harness can persist both artifacts independently

### Phase 3: Add Prompt Modes

Goal:

- make runtime intent explicit

Includes:

- add `interactive_coding`
- add `subagent`
- add `analysis`

Definition of done:

- different modes produce different prompt assemblies
- subagent runs no longer reuse the exact same prompt as the main session

### Phase 4: Add Tool Guidance Sections

Goal:

- let tools contribute behavioral prompt content

Includes:

- tool-level instruction hooks
- initial support for:
  - `Sleep`
  - `CreateTask`
  - `ExecCommand`
  - `EditFile`

Definition of done:

- tool guidance is no longer hardcoded only in the global prompt
- new tools can add prompt behavior without editing the main runtime prompt

### Phase 5: Add Prompt Inspectability

Goal:

- make prompt debugging first-class

Includes:

- `debug prompt`
- `debug prompt --mode ...`
- artifact emission for benchmark runs

Definition of done:

- any run can produce the exact effective prompt seen by the model
- prompt changes can be diffed between benchmark runs

## Immediate Prompt Content Improvements

Once the architecture is extracted, these content improvements should be made.

### 1. Stronger Project/Code Analysis Guidance

Add explicit guidance for:

- read before changing
- distinguish explanation from modification
- summarize existing structure before proposing roadmap changes
- cite concrete file/module evidence when analyzing a codebase

This is especially important for analysis tasks where current prompt behavior is
still too generic.

### 2. Stronger Verification Contract

The prompt should more explicitly require:

- run verification when a task changes code
- report failed verification honestly
- distinguish “I changed code” from “the task is complete”

### 3. Stronger Tool-Selection Heuristics

The prompt should more explicitly prefer:

- `ReadFile` over shell reads
- `SearchText` over shell grep
- file tools before shell where possible
- shell only when validation or system execution is actually needed

### 4. Stronger Task Delegation Guidance

The prompt should explain:

- when bounded child-agent delegation is appropriate
- when not to create a task
- that the caller remains responsible for the final answer

## What Should Wait

The first prompt architecture iteration should not try to do everything.

Wait on:

- model-generated prompt sections
- prompt caching infrastructure
- dynamic MCP instruction injection
- interactive question/answer prompt flows
- large numbers of prompt modes

Those are easier to justify after the benchmark harness exists.

## Relationship To Benchmarking

This roadmap should be implemented in lockstep with the benchmark harness.

The intended workflow is:

1. establish baseline benchmark results with current prompt
2. implement prompt architecture extraction
3. rerun the same benchmark corpus
4. change prompt content in small steps
5. compare success, latency, and tool-loop behavior after each change

Prompt changes without benchmark feedback should be avoided.

## Implemented First Pass

The first pass of this roadmap is now implemented in:

- `src/prompt.rs`
- `src/context.rs`
- `src/runtime.rs`
- `src/main.rs` via `debug prompt`

What is now in place:

- explicit prompt assembly via `EffectivePrompt`
- `PromptMode` with:
  - `interactive_coding`
  - `headless_daemon`
  - `subagent`
  - `analysis`
- stable system sections
- separate dynamic context sections
- tool guidance sections for:
  - `Sleep`
  - `CreateTask`
  - `ExecCommand`
  - `EditFile`
- prompt inspectability via:
  - `cargo run -- debug prompt ...`

What changed in content:

- stronger code-analysis guidance
- stronger verification contract
- stronger file-tool-over-shell preference
- explicit finishing contract:
  - provide the user-facing summary before calling `Sleep`

## Tool Priority After Prompt Work

Prompt architecture should come before adding broad new tool surface.

After prompt work begins, the only near-term tool additions worth considering
are:

- `TodoWrite`
- `TaskList`
- `TaskStatus`
- `TaskStop`

These help coding-task orchestration without introducing new external
dependencies or new benchmark variables.

The following should stay out of the first prompt benchmark wave:

- `WebFetch`
- `MCP`
- `AskUserQuestion`
- `LSP/diagnostics`

## Success Criteria

This roadmap is successful when:

- Holon prompt structure is inspectable
- prompt changes can be benchmarked against a stable corpus
- analysis tasks become less generic and more code-grounded
- coding tasks show better tool selection and cleaner convergence
- new tool guidance can be added without bloating one global string
