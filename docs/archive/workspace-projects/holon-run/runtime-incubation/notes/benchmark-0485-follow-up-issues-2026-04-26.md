# Benchmark 0485 Follow-Up Issues

Date: 2026-04-26

Context:

- Benchmark case: `runtime-incubation-0485-prompt-context-snapshot-coverage`
- Claude run:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-26-0485-2026-04-26T02-20-14-735Z/runtime-incubation-0485-prompt-context-snapshot-coverage/claude-cli/run-01`
- pre-public runtime run:
  `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/.benchmark-results/anthropic-live-refresh-2026-04-26-0485-runtime-incubation-only-2026-04-26T02-57-27-885Z/runtime-incubation-0485-prompt-context-snapshot-coverage/runtime-incubation-anthropic/run-01`
- Main report:
  `projects/holon-run/runtime-incubation/notes/benchmark-0485-prompt-context-snapshot-coverage-2026-04-26.md`

This note is a discussion backlog, not an implementation plan. Items should be
promoted to GitHub issues only after the design choice is clear.

## 1. ApplyPatch Patch Format Mismatch

Status: RFC recorded; implementation issue created.

Evidence:

- pre-public runtime Anthropic exposed `ApplyPatch` as JSON tool input with `patch: string`.
- The model repeatedly wrote unified-diff-like patch bodies.
- The current runtime expected the Codex-style `*** Begin Patch` DSL.
- Invalid `ApplyPatch` calls caused fallback shell editing with `cat`, `sed -i`,
  and temporary Python scripts.

Current decision:

- Make unified diff the only model-visible patch body format.
- Keep provider-specific outer tool shape:
  OpenAI/Codex custom/freeform grammar; Anthropic JSON `patch`.
- Do not preserve the legacy DSL as a public/runtime contract.

References:

- RFC: `docs/rfcs/apply-patch-unified-diff-contract.md`
- Issue: https://github.com/holon-run/runtime-incubation/issues/505

## 2. Final Message Does Not Reflect Full Work Item

Status: Needs design discussion.

Evidence:

- pre-public runtime did add snapshot tests in the final diff.
- The final message only summarized the last continuation that fixed `#[test]`
  annotations and warnings.
- This made the run look like it produced only a tiny hygiene patch, even though
  the actual diff contained broader test additions.

Problem framing:

- Final/result generation appears to summarize the most recent continuation
  instead of the whole work item delivery.
- A late local fix can overwrite the narrative of the main implementation.

Discussion questions:

- Should final generation read from work item history, commit metadata, diff
  summary, or accumulated result briefs?
- Should runtime distinguish "delivery summary" from "latest continuation
  result"?
- Should final message include a required mapping to user/request acceptance
  criteria?

## 3. Continuation After Commit / PR Creation

Status: Needs design discussion.

Evidence:

- pre-public runtime committed, pushed, and attempted PR creation, then continued processing
  the active work item.
- The follow-up work fixed real warnings, so the continuation was not purely
  useless.
- The final summary still became scoped to that continuation rather than the
  whole delivery.

Problem framing:

- Runtime needs clearer semantics for "post-delivery verification/fixup" versus
  "continue active work".
- The system should not treat a narrow post-delivery fix as the whole work item
  outcome.

Discussion questions:

- What event should mark a work item as delivery-complete?
- Should commit/PR creation create a delivery checkpoint?
- Should post-delivery continuation be allowed but forced to append to the
  existing delivery summary instead of replacing it?

## 4. Shell Editing Fallback After Tool Failure

Status: Partly addressed by ApplyPatch RFC; still needs behavior discussion.

Evidence:

- After `ApplyPatch` failures, the agent used shell rewrite tactics:
  `cat >>`, `sed -i`, and temporary Python scripts.
- This increased command count, token usage, and introduced secondary formatting
  or annotation mistakes.

Problem framing:

- The runtime/prompt says ApplyPatch should be the mutation primitive, but tool
  failure recovery still allows low-quality shell edits.
- Some shell edits are legitimate, but fallback should not become the default
  recovery path after a patch syntax error.

Discussion questions:

- Should certain shell mutation patterns be discouraged by prompt only or
  guarded by runtime?
- Should an `ApplyPatch` syntax failure produce a stronger retry hint that asks
  for one corrected patch before shell fallback?
- Should benchmark metrics explicitly count shell mutation fallback?

## 5. Command Granularity Is Too Fine

Status: Needs design discussion.

Evidence:

- pre-public runtime used 153 shell commands versus Claude's 31.
- Many commands were small `sed`, `head`, `grep`, `cargo`, and inspection steps.
- Each command generally created another model round and amplified fixed context
  cost.

Problem framing:

- The agent made progress, but the tool loop was too chatty.
- The issue is not just "too much exploration"; implementation and debugging
  were split into many single-command turns.

Discussion questions:

- Should prompt guidance prefer batched local inspection once the target file is
  known?
- Should the runtime surface command-loop diagnostics when many read-only or
  verification commands happen without meaningful diff changes?
- Should there be a model-visible checkpoint asking for the next edit plan after
  N low-progress tool rounds?

## 6. Prompt Cache / Input Token Instability

Status: Needs measurement and design discussion.

Evidence:

- pre-public runtime input tokens were much higher than Claude.
- Some high-input pre-public runtime rounds had no provider cache read.
- Large tool outputs, changing prompt sections, or provider payload instability
  may have reduced cache reuse.

Problem framing:

- Token usage is not only a model verbosity issue.
- Repeated provider turns with unstable large payloads can dominate input cost.

Discussion questions:

- Which prompt sections are stable versus per-round dynamic in Anthropic runs?
- Are tool results included in a cache-unfriendly position?
- Should large tool receipts be compacted or referenced by artifact id earlier?
- Should benchmark reports include cache-read/cache-miss distribution by round?

## 7. Verifier Does Not Measure Issue Coverage

Status: Needs benchmark design discussion.

Evidence:

- `cargo test` passed, but it did not judge whether the snapshot matrix was
  broad enough for the issue intent.
- Claude and pre-public runtime both passed verifier while producing different coverage
  breadth.

Problem framing:

- Test pass is necessary but insufficient for issue-driven benchmark quality.
- Some cases need artifact-level checks or reviewer-style evaluation of
  acceptance criteria.

Discussion questions:

- Should benchmark manifests support case-specific artifact probes?
- Should issue acceptance criteria be converted into a lightweight coverage
  checklist?
- Should reviewer evaluation be a separate score from verifier pass/fail?
