# Holon Benchmark Guardrails for Structural Refactors

This document defines SVS-305: the minimal benchmark guardrail set that must
stay green while structural refactors land.

## Purpose

Structural refactors (SVS-301 through SVS-304) will change internal module
boundaries without intentionally changing runtime semantics. These benchmarks
serve as the safety rail that confirms semantic behavior is preserved.

## Why A Separate Guardrail Document

The full benchmark corpus (`docs/benchmark-results.md`) documents historical
comparisons against Claude SDK and highlights many interesting behavioral
differences.

This document is narrower. It answers only one question:

**Which small, fast benchmarks must pass to confirm that a structural refactor
did not break core Holon semantics?**

## Guardrail Criteria

A benchmark belongs in the guardrail set only if it:

1. **Tests core semantics** - exercises essential runtime behavior that
   structural refactors might accidentally break
2. **Runs quickly** - completes in under 60 seconds for both runners, making it
   practical to run frequently during refactors
3. **Is stable** - has demonstrated consistent passing behavior in recent
   benchmark runs
4. **Covers diverse paths** - represents different major runtime modes:
   - interactive coding
   - analysis
   - follow-up context retention
   - coordination

## The Guardrail Set

### G1: Single-Turn Coding Fix

**Task:** `fix-greeting-preserves-case`

**What it tests:**
- Basic coding loop: read file, identify bug, edit, verify, finish
- Proper final delivery with "what changed + why + verification"
- Interactive coding mode with tool profile `coding`

**Why it's a guardrail:**
- Structural refactors to `runtime.rs` or tool dispatch could break the
  turn-execution loop
- Changes to final delivery logic could regress result quality
- Tool-layer refactors could affect `EditFile` or `ExecCommand` behavior

**Success criteria:**
- `verify_exit_code: 0` (test passes)
- `max_files_changed: 2` (minimal, targeted fix)
- Final message explains the root cause and verification

**Reference implementation:**
- `benchmark/tasks/fix-greeting-preserves-case.json`
- `benchmark/fixtures/greeting-bug/`

---

### G2: Multi-Turn Follow-Up Context Retention

**Task:** `followup-greeting-context`

**What it tests:**
- Session maintains context across turns
- Follow-up answers are grounded in actual edits and verification
- Brief history and tool execution history survive turn boundaries

**Why it's a guardrail:**
- Splitting session lifecycle from turn execution (SVS-301) risks losing
  cross-turn context
- Extraction of final delivery logic (SVS-302) could break brief persistence
- Provider turn contract changes (SVS-304) might affect context reconstruction

**Success criteria:**
- `verify_exit_code: 0`
- Final answer contains: `greeting.js`, `tolowercase`, `node test.js`
- Correctly identifies the changed file, root cause, and verification command

**Reference implementation:**
- `benchmark/tasks/followup-greeting-context.json`
- `benchmark/fixtures/greeting-bug/`

---

### G3: Coordination With Planning

**Task:** `coordination-sequential-render-plan`

**What it tests:**
- Multi-step planning with `TodoWrite`
- Session coordination across multiple edits
- Follow-up reporting on completed vs pending steps
- Verification after multiple changes

**Why it's a guardrail:**
- Tool-layer refactors (SVS-303) could break `TodoWrite` or related tools
- Session orchestration changes might affect task/todo state management
- Tests that stateful coordination survives structural changes

**Success criteria:**
- `verify_exit_code: 0`
- `min_final_message_length: 160`
- Final answer contains "completed", mentions "pending" or "none", and
  references `node test.js`

**Reference implementation:**
- `benchmark/tasks/coordination-sequential-render-plan.json`
- `benchmark/fixtures/sequential-render-bugs/`

---

### G4: Open-Ended Analysis

**Task:** `analysis-runtime-architecture`

**What it tests:**
- Read-only analysis mode with tool profile `read_only`
- Multi-file evidence gathering and synthesis
- Structured reporting: current state, findings, recommendations
- No edits made during analysis

**Why it's a guardrail:**
- Provider turn contract changes (SVS-304) directly affect prompt/context
  assembly for analysis mode
- Tool spec/dispatch/execution split (SVS-303) could affect read-only tool
  behavior
- Analysis uses a distinct code path that structural changes might disrupt

**Success criteria:**
- `max_files_changed: 0` (no edits)
- `min_final_message_length: 400`
- Structured output covering current state, concrete findings, and
  recommendations

**Reference implementation:**
- `benchmark/tasks/analysis-runtime-architecture.json`
- `benchmark/fixtures/analysis-runtime/`

---

### G5: Bounded Synthesis

**Task:** `bounded-synthesis-analysis-runtime`

**What it tests:**
- Explicitly bounded synthesis task with length constraint
- Concise, grounded answers without over-exploration
- Turn-scoped bounded-output contract activation

**Why it's a guardrail:**
- Tests that mode-specific prompt handling survives structural changes
- Provider turn changes could affect how bounded-output guidance is injected
- Verifies that efficient synthesis behavior is preserved

**Success criteria:**
- `max_files_changed: 0`
- `max_final_message_length: 1500`
- Grounded file references with concrete findings

**Reference implementation:**
- `benchmark/tasks/bounded-synthesis-analysis-runtime.json`
- `benchmark/fixtures/analysis-runtime/`

---

## Running The Guardrail Set

### Quick Guardrail Check

Run all guardrail benchmarks once against the default model:

```bash
# From repo root
cd benchmark
npm run guardrails -- --runner holon \
                     --label guardrail-check-$(date +%s)
```

### Guardrail Comparison

Compare Holon against Claude SDK on the guardrail set:

```bash
cd benchmark
npm run guardrails -- --runner holon \
                     --runner claude_sdk \
                     --label guardrail-comparison-$(date +%s)
```

### Pre-Refactor Baseline

Before starting a structural refactor, establish a baseline:

```bash
cd benchmark
npm run guardrails -- --runner holon \
                     --repetitions 3 \
                     --label pre-SVS-XXX-baseline
```

Then after the refactor, compare:

```bash
npm run run compare --baseline pre-SVS-XXX-baseline \
                    --candidate post-SVS-XXX-refactor
```

## What "Staying Green" Means

For structural refactors, a guardrail benchmark "stays green" when:

### Strict Criteria (Required)

1. **Task success is preserved**
   - `success: true` in `metrics.json`
   - Verification exit codes match the task definition
   - No regression in success rate across multiple runs

2. **Semantic behavior is preserved**
   - Follow-up answers remain grounded in actual session history
   - Analysis tasks still produce structured, file-grounded reports
   - Coding tasks still verify and explain root causes

3. **No major efficiency regressions**
   - Duration does not increase by more than 50%
   - Tool call count does not increase by more than 30%
   - Model rounds do not increase by more than 2x

### Allowable Variance

The following variations are acceptable during structural refactors:

1. **Small efficiency changes** - ±10% duration or tool count variance is
   acceptable if success is preserved
2. **Implementation detail changes** - different but equivalent tool sequences
   that achieve the same result
3. **Final message wording changes** - as long as required substrings and
   structure are preserved

### Blocking Regressions

These regression types should block a structural refactor from landing:

1. **Success regression** - any guardrail task that previously passed now fails
2. **Context loss** - follow-up answers no longer grounded in actual history
3. **Verification failure** - coding tasks that stop verifying or break
   verification
4. **Mode collapse** - analysis tasks stop producing structured reports or
   collapse to generic completions

## Guardrails For Specific Refactors

### SVS-301: Extract Turn Execution From `runtime.rs`

**Most relevant guardrails:**
- G1 (single-turn coding)
- G4 (analysis)

**Risk:** Breaking the core model/tool loop

**What to watch for:**
- Turn execution still completes
- Tool calls still dispatch correctly
- Provider interaction still produces results

### SVS-302: Extract Final Delivery Logic From `runtime.rs`

**Most relevant guardrails:**
- G1 (single-turn coding - final delivery quality)
- G2 (follow-up context)

**Risk:** Breaking result derivation or brief shaping

**What to watch for:**
- Final messages still include "what changed + why + verification"
- Follow-up answers remain grounded
- No regression to weak "Completed." endings

### SVS-303: Split Tool Spec / Dispatch / Execution

**Most relevant guardrails:**
- G1 (tool calls during coding)
- G3 (TodoWrite and coordination tools)
- G4 (read-only tools)

**Risk:** Breaking tool routing or execution

**What to watch for:**
- All tools still dispatch correctly
- Tool inputs/outputs are preserved
- Read-only vs coding tool profiles still work

### SVS-304: Separate Provider Turn Contract From Session Orchestration

**Most relevant guardrails:**
- G2 (multi-turn context retention)
- G4 (analysis mode prompt/context)
- G5 (bounded synthesis)

**Risk:** Breaking context construction or mode-specific handling

**What to watch for:**
- Cross-turn context is preserved
- Analysis mode still produces structured reports
- Bounded-output contracts still activate correctly

## Adding New Guardrails

New benchmarks should be added to the guardrail set only when:

1. A structural refactor exposes a gap not covered by existing guardrails
2. The benchmark meets the guardrail criteria (fast, stable, semantic)
3. Both Holon and Claude SDK can pass it consistently

Do not add guardrails for:
- Edge cases that don't affect core semantics
- Tasks that are unstable or flaky
- Benchmarks that take longer than 60 seconds

## Relationship To Full Benchmark Suite

The guardrail set is a subset of the full benchmark corpus.

**Full benchmark suite purposes:**
- Compare Holon vs Claude SDK across many dimensions
- Explore behavioral differences in follow-up, coordination, analysis
- Guide prompt and runtime improvements
- Support exploratory benchmarking

**Guardrail set purposes:**
- Provide a fast, stable safety check for structural refactors
- Confirm core semantics are preserved
- Run frequently during refactors without excessive iteration time

When working on structural refactors:
1. Run the guardrail set frequently (after each meaningful change)
2. Run the full benchmark suite occasionally (before/after major milestones)
3. Treat any guardrail regression as a blocking issue
4. Use full-benchmark regressions to diagnose deeper issues

## Current Status

The guardrail set is currently defined but not yet automated as a pre-commit
or CI check. The next step (SVS-306) would be to integrate these into a
runnable guardrail command that can be called automatically.

## Definition Of Done

SVS-305 is complete when:

- [x] Guardrail criteria are documented
- [x] The minimal guardrail set is defined
- [x] Each guardrail includes:
  - What it tests
  - Why it's a guardrail
  - Success criteria
  - Reference implementation path
- [x] Usage instructions show how to run guardrails for specific refactors
- [x] Relationship to the full benchmark suite is clarified
- [ ] (Optional) Guardrail check is automated or integrated into workflow
