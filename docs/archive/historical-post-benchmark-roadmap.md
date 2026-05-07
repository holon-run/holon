# Holon Post-Benchmark Roadmap

This document records the focused roadmap that followed the first benchmark
waves against Claude Agent SDK, and the result of executing that roadmap.

It is narrower than:

- `docs/roadmap.md`
- `docs/coding-roadmap.md`
- `docs/prompt-architecture-roadmap.md`

Those documents describe the broad runtime and coding-agent direction. This
document describes the benchmark-driven refinement phase that came after the
first working Holon baseline.

## Why A Separate Roadmap

At this point, Holon was no longer blocked on basic coding-loop capability.

The important open questions were:

- how to improve open-ended analysis quality
- how to compare Holon vs Claude SDK fairly
- how to avoid overreacting to raw tool counts
- which small tools were actually worth adding next

That required a narrower roadmap:

- analysis capability first
- benchmark visibility second
- small coordination tools third
- tool-surface comparison fourth
- final delivery quality last

## Guiding Rules

### 1. Enhance Analysis First

Improve analysis capability before making strong claims about efficiency.

### 2. Do Not Prematurely Label Over-Reading As A Bug

Higher tool counts can come from:

- prompt/runtime behavior
- tool granularity
- benchmark measurement gaps
- Claude SDK tool-surface advantages

So capability and observability should improve before drawing a hard
conclusion.

### 3. Benchmark-Grounded Decisions

Prompt/runtime/tool changes in this phase should be justified by:

- benchmark artifacts
- code references
- comparison runs

### 4. No Task-Specific Prompt Tricks

Prompt changes in this phase must remain general:

- stronger analysis contracts
- stronger evidence expectations
- stronger synthesis/finishing contracts
- better mode-specific heuristics

But not:

- fixture-specific instructions
- benchmark-name references
- output hacks written only to satisfy one checker

### 5. Preserve Inspectability

Every improvement should remain visible in:

- prompt dumps
- tool traces
- transcripts
- benchmark metrics
- final message artifacts

## Progress Snapshot

- `PB1`: completed
- `PB2`: completed
- `PB3`: completed
- `PB4`: completed
- `PB5`: completed

## PB1: Analysis Capability V1

### Goal

Improve Holon's open-ended analysis capability before making strong claims
about analysis inefficiency.

### Includes

- strengthen analysis-mode prompt guidance
- improve grounded current-state summaries
- improve findings/recommendations structure
- preserve long-form result quality across truncation and completion paths
- keep analysis guidance generic rather than benchmark-shaped

### Result

Completed.

Key changes:

- strengthened `PromptMode::Analysis` in `src/prompt.rs`
- added analysis-oriented tool guidance for:
  - `ListFiles`
  - `SearchText`
  - `ReadFile`

Validation:

- `pb1-roadmap-audit-v1`

Observed outcome:

- roadmap-audit output became more grounded
- recommendations were less likely to repeat already completed work
- the result was still somewhat terse, which was addressed again in `PB5`

## PB2: Benchmark And Comparison Metrics V2

### Goal

Improve benchmark visibility before deciding whether Holon is truly
over-reading or merely using a coarser tool surface.

### Includes

- richer benchmark metrics
- better artifact-level inspectability
- runner comparison that goes beyond raw tool count

### Result

Completed.

Implemented in `benchmark/run.mjs`:

- `read_ops`
- `search_ops`
- `list_ops`
- `exec_ops`
- `create_task_ops`
- `sleep_ops`
- `unique_files_read`
- `unique_search_queries`
- `bytes_read`
- `search_to_read_chains`

Validation:

- `pb2-metrics-roadmap-audit-v2`
- `pb2-metrics-roadmap-audit-v3`

Observed outcome:

- the clean comparison run (`v2`) showed Holon and Claude SDK performed almost
  the same number of file reads on roadmap audit
- that removed the basis for a simple "Holon just reads too much" claim
- the richer metrics shifted the question toward:
  - read granularity
  - search/discovery strategy
  - tool-surface differences

## PB3: Analysis-Oriented Tooling

### Goal

Add a small set of coordination tools that strengthen analysis/coding
workflows without expanding into larger product surface area.

### Includes

- `TodoWrite`
- `TaskList`
- `TaskStatus`
- `TaskStop`

### Result

Completed.

Implemented:

- `TodoWrite`
- `TaskList`
- `TaskStatus`
- `TaskStop`

Related runtime/storage changes:

- todo snapshots are persisted in storage
- the latest todo snapshot is included in context construction
- running tasks can be cancelled through runtime-managed task handles
- HTTP read path added for latest todos

Validation:

- `cargo test`
- `tests/runtime_flow.rs`

Observed outcome:

- coordination tools are now real runtime primitives rather than schema-only
  placeholders
- they support longer analysis/coding sessions without introducing network,
  MCP, or IDE dependencies

## PB4: Tool Surface Comparison Against Claude Agent SDK

### Goal

Make tool-surface differences a first-class part of the comparison, instead of
implicitly blaming prompt/runtime behavior for all metric gaps.

### Result

Completed.

Comparison report:

- `docs/tool-surface-comparison.md`

Observed outcome:

- no strong evidence that Holon's current analysis behavior is simply a bug
- Holon and Claude SDK performed similar numbers of file reads on roadmap audit
- the more meaningful current difference is:
  - Claude SDK does more discovery-style search/list work
  - Holon reads larger chunks once it commits to evidence

So the next refinement target is better evidence targeting, not blind pressure
to reduce tool count.

## PB5: Final Delivery And Follow-Up Quality

### Goal

After the analysis and measurement loop is stronger, improve final delivery and
follow-up quality across coding and analysis tasks.

### Includes

- preserve the existing finishing fixes
- strengthen quality for:
  - what changed
  - why it was broken
  - what should happen next
  - long-form analysis conclusions
- reduce weak low-information endings

### Result

Completed.

Key changes:

- strengthened reporting guidance in `src/prompt.rs`
- analysis mode now prefers a concise structured report rather than a stream of
  notes
- updated the roadmap-audit repo snapshot to include
  `docs/post-benchmark-roadmap.md`, preventing stale recommendations about a
  file that now exists
- repaired `config-merger-bug` fixture drift back to a genuinely failing state

Validation:

- `pb5-roadmap-audit-v2`
- `pb5-followup-greeting-v1`

Observed outcome:

- roadmap audit now produces a long structured report with:
  - current state
  - findings
  - prioritized improvements
- follow-up greeting context remains strong:
  - correct file
  - correct root cause
  - correct verification command

## Outcome

This refinement phase succeeded.

The combined result is:

- Holon analysis quality is stronger and more structured
- benchmark output is rich enough to reason about tool-surface differences
- small coordination tools now exist and are tested
- the Holon vs Claude SDK comparison is more honest and less count-driven
- final delivery quality improved without introducing benchmark-specific prompt
  hacks
