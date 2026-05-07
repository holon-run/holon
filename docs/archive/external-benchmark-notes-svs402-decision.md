# SVS-402: Decision on Adding a Stronger Exploration Tool

**Status**: Decided - Go/No-Go: **No-Go**

**Date**: 2026-04-03

**Depends On**: `SVS-401` (tool surface comparison completed)

## The Question

Should Holon add a new exploration-oriented tool to improve analysis performance, or should the project focus on improving strategy with the current tool set?

## Decision: No New Exploration Tool Now

**Verdict**: Do **not** add a new exploration-oriented tool at this time. Focus on strategy refinement instead.

## Rationale

### 1. Analysis ability is already competitive

Evidence from `SVS-401` shows that Holon's core analysis capability is not deficient:

- Both runners successfully complete the same analysis tasks
- Number of file reads is comparable (19 vs 19 in open-ended audit)
- Unique files touched is similar (18 vs 17)
- On focused tasks, Holon can achieve the goal with **fewer** tool calls than Claude SDK

This is not a "Holon cannot analyze" problem.

### 2. The gap is in synthesis strategy, not discovery surface

The key difference is not that Holon needs a better way to find files. The evidence shows:

- Holon searches less (`2` search ops vs `5` in open-ended audit)
- Holon reads more bytes once it commits (`204,757` vs `75,010` in open-ended audit)
- Holon uses more model rounds to synthesize (`8` vs `1` in one focused task)

The pattern is clear: Holon finds the relevant evidence, then:

- reads larger chunks per file
- spends more rounds reasoning across what it read
- produces output through multi-turn synthesis rather than single-turn extraction

A new "better discovery" tool does not address this. The fix is:

- narrower read targeting
- earlier stopping once evidence is sufficient
- cheaper synthesis when enough context already exists

### 3. Current tool surface already supports necessary exploration

Holon's current tool set already provides:

- `ListFiles` - for directory and glob-based discovery
- `SearchText` - for content search across files
- `ReadFile` - for pulling specific evidence

The SVS-401 comparison shows that Holon uses **fewer** discovery calls than Claude SDK, not more. The problem is not lack of discovery power.

### 4. Adding a new tool would add complexity without clear benefit

Before adding a new tool surface, the project should be able to answer:

- What concrete problem does it solve that current tools cannot?
- What benchmark evidence shows the current tools are the bottleneck?
- Will it reduce model rounds, or just add another exploration path?

Current evidence does not support "current tools are inadequate". Adding a new exploration tool would:

- increase implementation and testing surface
- add prompt complexity for marginal gain
- risk further rounds of "tool strategy" iteration without addressing the actual synthesis cost issue

### 5. The right next steps are strategic, not structural

The SVS-401 evidence points to these higher-leverage investments:

- **Read targeting**: improve heuristics for reading only what's necessary
- **Stopping discipline**: detect when enough evidence is gathered and synthesize directly
- **Synthesis efficiency**: reduce model rounds by emitting results more directly
- **Analysis mode clarity**: SVS-101 already started this; continue separating open-ended vs bounded analysis

These changes do not require new tools. They require sharper strategy and better prompting.

## What "No-Go" Means Concretely

### Do not do (now):

- Add a new "explore_project" or similar mega-tool
- Introduce a new discovery-oriented primitive without benchmark evidence it's needed
- Treat the current tool set as the primary explanation for analysis cost differences

### Do instead:

- Refine analysis prompt rules (SVS-101, SVS-103, SVS-104)
- Improve read granularity heuristics based on task mode
- Add benchmarks that expose synthesis round count as a first-class metric
- Consider narrower exploration helpers **only after** strategy refinements hit a clear ceiling

## Revisit Conditions

This decision should be revisited if:

1. Strategy refinements hit a clear ceiling on benchmark performance
2. A concrete pattern emerges where `ListFiles`/`SearchText`/`ReadFile` cannot express an efficient exploration
3. Benchmark evidence shows a specific class of tasks where a new tool would reduce **total** cost (tokens + rounds), not just tool count

## Evidence Basis

This decision is grounded in the comparison documented in `docs/tool-surface-comparison.md`:

- `pb2-metrics-roadmap-audit-v2` for open-ended audit baseline
- `svs401-compare-v1` for focused fresh comparison with token/round data
- Specific metrics: read ops, unique files, bytes read, model rounds, token counts

No hand-wavy "we should probably add an exploration tool" rationale is accepted. The decision follows the benchmark evidence.

## Next Work

Given this no-go decision, the next practical work should prioritize:

1. `SVS-101` - Split analysis prompt rules more explicitly by mode
2. `SVS-104` - Keep bounded output optimization strictly turn-scoped
3. `SVS-103` - Stabilize follow-up explanation quality

These tasks improve strategy without expanding the tool surface.

## Sign-Off

- **Issue**: `SVS-402`
- **Decision**: No-Go on new exploration tool
- **Basis**: SVS-401 comparison evidence
- **Next**: Focus on strategy refinement in Phase S2
