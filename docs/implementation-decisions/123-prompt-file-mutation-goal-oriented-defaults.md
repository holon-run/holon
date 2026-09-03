# Prompt file-mutation guidance is goal-oriented with scenario defaults

## Choice

The system prompt no longer mandates ApplyPatch mechanically for every file
mutation. `core_contract` states the goal (small, local, explicitly verifiable
mutations) and delegates primitive choice to the file-mutation guidance.
`tool_file_mutation.md` now states scenario defaults: edits to existing files
default to ApplyPatch; creating a new file may use ApplyPatch or a bounded
heredoc, whichever is cheaper and safer; very large, generated, or wholesale
changes follow the existing lower-context path.

## Reason

#2662 (Step 7): the blanket mandate added prompt pressure without improving
outcomes for new-file creation, where a bounded heredoc is equivalent. Keeping
the failure-mode rationale (explicit context-mismatch errors, structured
receipts) preserves the behavior we actually want instead of the mechanism
name.

## Preserved boundary

One negative guardrail stays: avoid in-place shell rewrites like `sed -i`,
because they exit successfully even when the pattern does not match and leave
no structured receipt. Unlike the removed Codex `*** Begin Patch` prior (a
format error disproven by receipts), this is a path-choice error with no
receipt fallback, so the warning remains while the regular option list keeps
only patch and bounded heredoc.
