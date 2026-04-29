---
title: RFC: Turn-Local Context Compaction
date: 2026-04-24
status: draft
---

# Turn-Local Context Compaction For Holon

This note describes a focused compaction design for one specific failure mode:

- the agent stays inside one runtime turn
- the provider/tool loop runs for many rounds
- the provider conversation grows too large before the turn ends

This is different from Holon's existing cross-turn compaction.

Existing cross-turn compaction is handled before prompt assembly through
`src/context.rs::maybe_compact_agent()`. That logic compacts durable message
history. It does **not** compact the in-memory provider conversation that is
built inside `src/runtime/turn.rs::run_agent_loop()`.

The result is that Holon can still exceed context inside a single turn even if
long-lived session memory is already under control.

## 1. Problem Shape

The current turn loop behaves roughly like this:

1. start round 1 with one rendered context attachment
2. send the prompt to the provider
3. append assistant blocks to in-memory `conversation`
4. execute tools
5. append tool results to in-memory `conversation`
6. build the next continuation request from the full accumulated conversation

Relevant code paths today:

- `src/runtime/turn.rs:76`
- `src/runtime/turn.rs:136`
- `src/runtime/turn.rs:195`
- `src/runtime/turn.rs:496`

So the dangerous growth is not mainly:

- append-only durable logs
- old operator messages
- old cross-turn session context

The dangerous growth is:

- assistant tool-planning text repeated over many rounds
- repeated tool result reinjection inside the same turn
- command-family previews and task outputs that are individually bounded but
  still accumulate over many rounds

## 2. New Assumption

This design assumes two other contracts are already in place:

1. tool returns are standardized at the outer envelope layer
2. command-family large outputs are already bounded inside their own result
   payloads

That means a tool result now has stable machine-readable anchors such as:

- `tool_name`
- `status`
- `summary_text`
- structured `result`
- structured `error`

And command-family payloads can expose bounded fields such as:

- preview text
- `truncated`
- artifact references
- artifact indices

This is what makes deterministic turn-local compaction practical. Without this,
the runtime would be compacting opaque strings.

## 3. Goal

The goal is:

> Keep one logical runtime turn alive across many provider/tool rounds by
> compacting older in-turn conversation into deterministic round summaries,
> while preserving the most recent exact interaction tail.

This implies:

- no extra LLM compaction call
- no mutation of durable audit history
- no need to restart the turn just because it grew large
- no requirement that every tool family implement the same truncation contract

## 4. Scope

This design is specifically for:

- in-memory provider conversation inside one `run_agent_loop()`

It is not the design for:

- cross-turn message compaction
- working memory / episode memory
- transcript archival
- operator-facing session reset

Those remain covered by:

- `docs/rfcs/long-lived-context-memory.md`
- `docs/single-agent-context-compression.md`

## 5. Design Summary

Turn-local compaction should introduce two representations of the current turn:

1. a canonical in-turn ledger
2. a bounded provider projection

The canonical ledger stores the complete semantic record of completed rounds
inside the current turn.

The provider projection is what gets sent back to the model on continuation
requests.

The runtime should stop treating `conversation` as both:

- the canonical source of truth
- the model-visible continuation payload

Those two roles should be separated.

## 6. Canonical Turn Ledger

The turn loop should accumulate a typed record per completed provider/tool
round.

Suggested shape:

```rust
struct TurnRoundRecord {
    round: usize,
    assistant_text_blocks: Vec<String>,
    tool_calls: Vec<ToolCall>,
    tool_results: Vec<ToolOutputEnvelope>,
    stop_reason: Option<String>,
}
```

This does not need to replace durable storage. It only needs to exist for the
life of one runtime turn.

Important detail:

- the ledger should keep typed tool results, not only rendered provider strings
- the ledger should group by completed round, not by individual appended
  message fragment

Round is the right compaction unit because the provider typically sees one
logical cycle:

- assistant response
- tool calls
- tool results

Compacting raw blocks independently would break that meaning.

## 7. Bounded Provider Projection

Before every continuation request after round 1, Holon should build a fresh
provider projection from:

1. initial rendered context attachment
2. one synthetic turn-local recap block for older compacted rounds
3. an exact recent tail of retained rounds

Conceptually:

```text
initial context
+ compacted round recap
+ recent exact round tail
```

The recap should be deterministic and runtime-authored, not model-authored.

The recent tail should stay exact because it is the highest-value region for:

- immediate follow-up reasoning
- chained tool use
- short-term error recovery
- preserving local coherence

## 8. Compaction Unit And Retention Rule

The compaction unit should be one completed round.

The retention rule should be:

- keep the most recent `N` completed rounds exactly
- compact older completed rounds into one recap
- never compact a partially processed current round

Suggested first-pass default:

- keep the most recent 2 completed rounds exactly

Why not keep only 1:

- many coding flows need one round of setup plus one round of verification
- the most recent tool result often depends on the assistant text from the
  previous round

Why not keep 5 by default:

- if large command/task previews exist, 5 exact rounds can still be too large

## 9. Deterministic Round Recap

The recap should summarize older rounds using structured runtime evidence.

Each recap line should be derived from:

- assistant text summary
- tool names
- tool result `summary_text`
- stable handles and ids
- bounded error fields

Suggested recap shape:

```text
Turn-local recap for rounds 1-6:
- Round 1: identified daemon startup instability; ran ExecCommand; summary: command exited with status 0
- Round 2: inspected task state; TaskStatus summary: task task_42 status retrieved
- Round 3: waited on command task; TaskOutput summary: output retrieved; artifact: /tmp/...
- Round 4: updated work item; summary: marked verification active
- Round 5: verification failed; ExecCommand error: execution_root_violation; recovery_hint: omit workdir
- Round 6: retried verification in active workspace; summary: command exited with status 1
```

The recap should preserve:

- command/task handles
- child agent ids
- work item ids when present
- `error.kind`
- `error.message`
- `error.recovery_hint`

The recap should not preserve:

- full preview bodies already replaced by artifact refs
- repeated bounded stdout/stderr previews from old rounds
- verbose assistant narration that no longer affects immediate follow-up

## 10. Trigger And Budget Rule

Turn-local compaction should be budget-driven, not round-count-driven alone.

Before every continuation request after round 1, the runtime should:

1. build the current projected conversation
2. estimate tokens for the continuation request
3. compare against a soft limit
4. compact older retained rounds until the request falls below target

Suggested controls:

- `turn_local_compaction_soft_limit_estimated_tokens`
- `turn_local_compaction_target_estimated_tokens`
- `turn_local_keep_recent_rounds`

The relationship should be:

- if estimated continuation size is below soft limit, do nothing
- if above soft limit, compact until below target

This avoids compacting on every round while still acting before provider hard
failure.

## 11. Command-Family Specific Rule

Since the main token pressure comes from command-family outputs, the recap
builder should understand command-family payloads explicitly.

For `ExecCommandResult` and `TaskOutputResult`:

- keep `summary_text`
- keep stable task ids and artifact indices
- keep a short preview only in the exact recent tail
- omit old preview text once a round has been compacted into recap form

That means older command-family rounds should collapse toward:

- outcome summary
- exit status when important
- task handle or artifact path when needed
- error and recovery hint when present

not:

- old stdout/stderr preview text
- repeated task output preview text

## 12. Projection Algorithm

Suggested runtime algorithm:

### Step 1: Accumulate canonical round records

After each provider round and tool execution phase, append a `TurnRoundRecord`
to a turn-local ledger.

### Step 2: Mark retained exact tail

Select the newest `turn_local_keep_recent_rounds` completed rounds as the exact
tail.

### Step 3: Compact older completed rounds

If token estimate is too large, convert older completed rounds into one recap
text block.

The recap should be regenerated from the canonical ledger, not incrementally
edited in place.

### Step 4: Rebuild provider conversation

The provider continuation payload should be rebuilt fresh from:

- initial user context attachment
- optional synthetic recap text
- exact assistant/tool interaction blocks for the retained tail

### Step 5: Retry estimate

If still above target:

- reduce retained tail size only if above the configured minimum
- otherwise apply stricter preview budgets to retained command-family results
- if still above target, fail with an explicit runtime error instead of sending
  an oversized request

## 13. Synthetic Message Placement

The synthetic recap should be inserted as a runtime-authored user text block
ahead of the recent exact tail.

It should be clearly marked, for example:

```text
[Runtime-generated turn-local recap]
The following rounds occurred earlier in this same turn and were compacted to
keep the continuation within context budget:
...
```

This is important because the model should not mistake the recap for a fresh
operator message.

## 14. Why This Is Simpler Than Full Generic Projection

A fully generic tool-result compaction framework is possible, but it is not the
best first implementation.

The immediate pressure is narrower:

- provider conversation overflow inside one turn
- dominated by command-family outputs

So the first implementation should stay focused:

- canonical round ledger
- round-based recap
- command-family aware recap extraction
- exact recent tail retention

This gives most of the value without requiring every tool family to implement a
large-output projector immediately.

## 15. Relationship To Existing Holon Memory Model

This design fits the existing long-lived memory direction:

- cross-turn memory remains in working memory and episode memory
- turn-local compaction is only for the current hot loop

That means the hierarchy becomes:

- cross-turn compaction: `maybe_compact_agent()` and long-lived memory
- in-turn compaction: round recap inside `run_agent_loop()`

These two layers should not be merged.

## 16. Suggested Implementation Boundary

Keep the first implementation small.

Suggested boundary:

1. introduce `TurnRoundRecord`
2. introduce `build_turn_local_recap()`
3. introduce `project_turn_conversation()`
4. estimate tokens before continuation requests
5. compact older rounds only when needed
6. add tests for long multi-round command-heavy turns

Avoid in the first pass:

- provider-specific in-turn compaction behavior
- LLM-generated recap
- generalized per-tool family compaction plugins
- cross-turn memory redesign

## 17. Expected Benefits

If implemented correctly, this should:

- prevent most single-turn `context_length_exceeded` failures
- preserve immediate continuity better than dropping old raw blocks
- make long coding turns viable without forcing a new turn boundary
- keep recap deterministic and testable
- work especially well once command-family outputs are already standardized and
  bounded
- if provider-side `context_length_exceeded` still occurs, treat it as a
  fail-fast turn terminal instead of auto-continuing the same active work item

## 18. Open Questions

- Should the exact retained tail be 2 rounds by default, or 3 for coding-heavy
  models?
- Should recap include short assistant intent summaries, or only tool/result
  summaries?
- If the request is still too large after recap plus stricter preview budgets,
  should Holon hard-fail, or internally split the turn into a new queued
  follow-up turn?
- Should the recap text itself be stored in the turn transcript for debugging,
  or remain a derived ephemeral projection?

## 19. Recommendation

Implement turn-local context compaction as a deterministic round-based provider
projection inside `run_agent_loop()`.

Do not wait for a fully generic all-tool projection framework.

The first pass should target the real pressure point:

- standardize the outer tool result contract
- keep command-family outputs bounded
- compact older in-turn rounds into a runtime-authored recap
- keep the newest exact interaction tail uncompressed
