---
title: "RFC: Provider Round Admission Limits"
date: 2026-11-16
status: accepted
handle: rfc-provider-round-admission
---

# Provider Round Admission Limits

This contract implements issue #4192, the single follow-up implementation
entry for the single-response tool call floods recorded in #3787 and #4045.
The goal is bounded whole-round admission with an explicit failure before any
tool side effect, not lossless recovery: a response that exceeds a quota is
rejected as a unit instead of dispatching thousands of calls, rejecting them
one by one, and persisting a matching volume of results, audit, and progress.

Related contracts:

- [Command tool family](command-tool-family.md)
- [Bounded input consumption](bounded-input-consumption.md)
- [Runtime error taxonomy](runtime-error-taxonomy.md)

## Scope of "one round"

A round is **one provider response**, not a whole turn and not an
accumulation across provider requests. All transports, streaming and
non-streaming, converge on a fully parsed block list before the single
semantic gate in `RuntimeHandle::process_provider_response`
(`src/runtime/turn/execution.rs`); no per-transport duplicate checks are
added.

## Default quotas

The limits are centrally defined constants with headroom over observed
normal peaks (29 calls, 63,215 B arguments, 65,862 B round payload):
no per-agent or per-provider overrides, online tuning, or automatic
relaxation exist in the first version.

| Dimension | Limit | Measurement |
| --- | ---: | --- |
| Managed tool calls | 64 | Count of `ModelBlock::ToolUse` handed to Holon dispatch; legal, unknown, and malformed names all count |
| Per-call parsed arguments | 128 KiB (131,072 B) | `input: serde_json::Value` serialized as compact JSON; UTF-8 bytes |
| Tool-round visible payload | 256 KiB (262,144 B) | All `Text.text` bytes plus every managed call's `id` + `name` + argument bytes |

Equal-to-limit is accepted; greater-than-limit is rejected. Multibyte
characters count as bytes. Compact serialization only removes structural JSON
whitespace; it never trims or rewrites whitespace inside argument strings.
The visible payload quota applies only to rounds with at least one managed
tool call; pure-text rounds are not subject to it. Provider-owned tool
blocks, thinking, reasoning, signatures, and provider envelope data are not
counted and must not be dropped or rewritten to dodge existing validation.

Byte accounting uses a bounded counting writer that stops at the budget, so
rejection evidence reports either an exact count or an explicit
`count=at_least` lower bound; addition overflow is treated as over limit.
The accounting never allocates an unbounded serialization buffer for
measurement.

## Batch nesting boundary

`ExecCommandBatch` keeps its existing 1–12 item contract, publishes the same
array length constraint in its tool schema, and rejects oversized raw item
arrays before per-item typed parsing, recovery-hint scanning, or execution;
empty objects count as items. Two independent round quotas govern batches:

- the top-level managed call count still counts a batch wrapper as one call;
- the expanded work item count counts a non-batch managed call as one and a
  batch call as its raw `items` array length, with multiple batches merged.

Both limits are 64, so sixty-four batches of twelve items are rejected by
expansion while sixty-four ordinary calls still pass. A missing or non-array
`items` value keeps the existing bounded `invalid_tool_input` failure and
counts as one expanded item; admission never guesses or repairs arguments,
and shell contents are never inspected to estimate inner work.

## Admission position and failure contract

1. The gate runs at the shared response reception boundary for all managed
   tools: after the response is fully parsed, before any tool execution,
   command start, task creation, or control tool effect for the round, and
   before the round is persisted as replayable assistant history or any
   per-call record is created.
2. Any quota violation rejects the whole round with zero tool executions,
   including mixed responses that contain terminal control tools such as
   `CompleteWorkItem` or `WaitFor`.
3. The turn enters an explicit non-retryable failure terminal with the stable
   reason code `provider_round_limit_exceeded` and a fixed quota enum field
   distinguishing which limit was hit. The runtime does not automatically
   re-ask the model, switch providers, or replay the input; queue claims
   settle through the existing failure terminal path with no
   claimed/running residue.
4. The rejection produces exactly one round-level rejection summary (bounded
   well under 4 KiB) plus the regular failure terminal and brief. No
   per-call `ToolExecutionRecord`, `ToolResult`, or progress event is
   created. The summary carries only the trusted reason code, quota,
   limits, bounded counts with complete/lower-bound markers, and the round
   number; it never echoes tool names, argument bodies, degenerate text, or
   fabricated totals.
5. The rejected response does not enter later provider continuations as
   assistant or tool history, and no fake tool results are synthesized to
   satisfy protocol pairing. Existing transport diagnostics are unchanged
   and this contract does not claim generation or reception resources are
   bounded.
6. Side effects already executed by earlier rounds, their results, and the
   original authorization evidence are preserved untouched; tasks already
   started keep their ownership and lifecycle. Cancellation or restart
   racing an admission rejection still yields exactly one terminal, and
   recovery never re-dispatches the rejected round or replays its input.
7. Responses within the limits keep the existing permission, argument,
   truncation mutation, prompt budget, and exact-round validation unchanged.

## Non-goals

Lossless continuation, exact-round degradation, truncated tool results,
in-round checkpointing, cross-restart recovery, automatic correction or
retry, provider switching, generation-time stream cancellation, raw
HTTP/wire byte limits, thinking or signature quotas, JSON parser depth,
output token limits, transport OOM handling, sandbox or in-shell work limits
(#3980, #920), and cross-message flood prevention. Migrating the incident
issues closed does not by itself fix the first degeneration cause.

## Verification

The contract is covered by unit tests for every quota boundary (limit minus
one, equal, plus one), Unicode and nested-JSON argument measurement, batch
and expansion boundaries, and by runtime integration tests that verify zero
side effects and a single failure terminal for flood responses.
