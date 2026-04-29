# Agent Handover 2026-04-03

This document summarizes the recent Holon implementation thread so a new agent
can continue without rereading the full chat history.

## 1. Current Ground Truth

The main branch already contains all completed work up through:

- benchmark observability:
  - `SVS-001` to `SVS-004`
- analysis/synthesis stabilization:
  - `SVS-101` to `SVS-104`
- structural refactors:
  - `SVS-301` to `SVS-305`
- tool-surface decision:
  - `SVS-401` to `SVS-402`
- worktree runtime:
  - `WT-001` to `WT-005`
  - `WT-101` to `WT-105`
  - `WT-201` to `WT-204`

See:

- `docs/issue-backlog.md`

Do not re-open or re-implement the completed issues above unless a specific
regression is found.

## 2. What Was Repeated By Mistake

The previous thread accidentally re-assigned some work that was already done.

The clearest example was:

- `SVS-101`

`SVS-101` had already landed in `src/prompt.rs`, but a later Holon session was
still sent to implement it again. That attempt only produced an extra test file
and no meaningful new behavior.

Practical rule:

- before assigning work from `docs/issue-backlog.md`, verify the current main
  branch state first
- treat the backlog as a starting point, not as proof that an issue is still
  open

## 3. How Holon Was Used During This Thread

Before native worktree orchestration was finished, Holon was driven in external
worktrees:

1. create a dedicated git worktree outside the main repo
2. start a release `holon serve` in that worktree
3. give Holon one narrow task
4. review the result manually
5. if good:
   - clean up local runtime artifacts
   - commit in the worktree
   - cherry-pick to `main`
6. if bad:
   - discard the worktree result
   - either retry with a smaller task or finish manually

This mode worked reasonably well for narrow tasks, especially:

- docs updates
- benchmark harness improvements
- bounded refactors with clear file ownership

It did **not** work reliably for wider refactor tasks that required many small
edits across already-changing files.

## 4. Observed Holon Strengths

Holon was useful at:

- narrow refactors with clear boundaries
- benchmark and prompt iteration
- worktree/runtime tests with a small, explicit scope
- producing a workable first patch that could be manually tightened

In practice, Holon behaved like:

- a useful first-pass implementer
- not yet a trustworthy end-to-end closer

## 5. Observed Holon Weaknesses

### 5.1 Empty or low-value running sessions

Some sessions appeared `awake_running` for a long time without visible progress.

Important nuance:

- this did not always mean true deadlock
- sometimes the runtime was still consuming model rounds or text-only turns

Instrumentation was added so later investigation can inspect:

- `provider_round_completed`
- `text_only_round_observed`

These should be checked before labeling a session as "stalled".

### 5.2 Weak final convergence

Holon frequently got close to a correct patch but failed to:

- stop at the right time
- cleanly verify
- produce a strong final delivery

This was improved several times, but it is still not fully solved for longer
tasks.

### 5.3 Brittle editing

This is the most important unresolved technical weakness.

`Holon` currently edits files through a very fragile exact string replacement
tool:

- `ReadFile`
- `EditFile(old_text, new_text, replace_all?)`

The implementation is essentially:

- read the whole file
- exact `replacen` / `replace`
- fail with `old_text not found` when the match drifts

That design contributed directly to failed implementation attempts, especially
on broader structural tasks.

See:

- `src/tool/execute.rs`
- `docs/basic-tool-comparison.md`

## 6. Important Failed Attempt: `SVS-201`

`SVS-201` was attempted through a Holon worktree session and did **not** land.

Key facts:

- the session consumed many model rounds and many tokens
- it repeatedly hit:
  - `EditFile` misses
  - malformed patch attempts
  - compile failures
- it eventually rolled back its own file changes
- it ended with analysis text rather than a mergeable patch

Interpretation:

- the task was too wide
- the current edit primitive is too brittle
- retrying the same task unchanged is not recommended

If `SVS-201` is resumed, it should be split more narrowly or revisited after
editing primitives improve.

## 7. Why Tool Improvement Became The Next Big Theme

After comparing Holon with Claude Code and Codex, the most important conclusion
was:

- Holon does not mainly suffer from "missing lots of tools"
- Holon mainly suffers from weak **editing primitives**

High-level comparison:

- `ListFiles` / `SearchText` / `ReadFile`: usable, but coarse
- `ExecCommand`: already serviceable
- `EditFile`: clearly behind

Structural conclusion:

- file/discovery naming should align more with Claude-style canonical names
- complex editing should learn from Codex-style `ApplyPatch`

See:

- `docs/basic-tool-comparison.md`

## 8. New Open Issues Added Late In The Thread

The only remaining open work items from that comparison are:

- `SVS-403` Add canonical tool alias layer
- `SVS-404` Add `ApplyPatch` as the primary complex edit primitive

The recommended order is:

1. `SVS-403`
2. `SVS-404`

The earlier coding-loop hardening items were later verified and should now be
treated as completed backlog work:

- `SVS-201`
- `SVS-202`
- `SVS-203`
- `SVS-204`

## 9. Recommended Next Steps

If a new agent is picking up from here, the safest next sequence is:

1. implement `SVS-403`
2. implement `SVS-404`
3. re-evaluate the editing/runtime backlog after those land

Do **not** reopen the old `SVS-201` attempt unless a fresh regression appears.

## 10. Repo Hygiene Notes

There are several local-only research or scratch docs that should not be
blindly committed just because they exist in `docs/`:

- `docs/claude-code-reference.md`
- `docs/claude-vs-codex-for-holon.md`
- `docs/agent-thread-unification.md`
- `docs/next-phase-direction.md`
- `docs/triggering-and-liveness.md`

Check intent before committing them.

## 11. One-Line Handoff

The next agent should treat Holon as:

- already strong enough on benchmarking, worktree orchestration, and prompt
  mode structure
- still weak on edit robustness and final convergence
- ready for tool-surface alignment and `ApplyPatch`
