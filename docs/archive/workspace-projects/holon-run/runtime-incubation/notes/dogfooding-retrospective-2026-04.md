# pre-public runtime Dogfooding Retrospective

Date: 2026-04-06
Scope: `pre-public runtime` supervised dogfooding with `AgentInbox`-backed GitHub review wake flow
Source log: `notes/dogfooding-supervision-log-2026-04-05.md`

## Summary

This round of dogfooding was successful.

The meaningful outcome was not just landing five PRs. The bigger result was
that `pre-public runtime` completed a real supervised development loop:

- enter a managed worktree
- implement a real issue
- open a PR
- subscribe to GitHub review/comment events through `AgentInbox`
- sleep
- wake on real review activity
- read inbox items through the `agentinbox` CLI
- repair the PR
- push again
- return to waiting

Five issues were completed this way:

- `#32`
- `#15`
- `#19`
- `#31`
- `#33`

## What Worked

### 1. The end-to-end supervision loop is now real

`pre-public runtime` can now participate in its own development through a real review loop
instead of a synthetic local-only cycle.

The validated path is:

- operator assigns a real issue
- `pre-public runtime` develops in a managed worktree
- `pre-public runtime` opens a PR
- `AgentInbox` delivers GitHub review/comment activity as wake events
- `pre-public runtime` reads inbox state through `agentinbox inbox list/read`
- `pre-public runtime` repairs the PR and pushes updates

This is the most important result of the whole round.

### 2. Worktree-based development is usable under supervision

Earlier runs showed that `EnterWorktree` existed but was not sticky enough.
After moving to:

- clean supervision checkouts
- staged prompts
- explicit worktree gates

the agent reliably developed inside isolated worktrees and stopped polluting
the main checkout.

### 3. `TaskOutput` materially improved verification handling

Adding `TaskOutput` closed a real product gap.

Before that change:

- long-running verification promoted to background tasks
- the agent had to juggle `TaskList`, `TaskGet`, shell sleeps, and output files
- verification frequently degraded into task fan-out

After `TaskOutput`, background verification became much more manageable, even
if it is still not perfect.

### 4. Fixing prompt fidelity improved execution stability

One of the most important discoveries was that long operator prompts were not
being lost at ingress. They were being truncated during prompt assembly because
`current_input` was rendered through a preview path.

Fixing that had visible effects:

- long paths survived into model-visible context
- narrow operator instructions became more reliable
- follow-up repair prompts degraded less often

This was a product bug, not just a prompting weakness.

### 5. `AgentInbox` is a viable wake layer for review-driven work

For GitHub review/comment events, the following split now looks correct:

- `AgentInbox` owns source polling/materialization/activation
- `pre-public runtime` owns wake/runtime meaning
- the agent uses the `agentinbox` CLI to read the actual inbox item

This separation feels sound for long-running agent services.

## What Did Not Work Reliably

### 1. Verification remains the weakest phase

The most persistent failure mode was not initial implementation. It was
verification and post-verification behavior.

Typical problems:

- tests already passed, but the agent launched extra verification anyway
- output looked incomplete, so the agent invented another shell command
- long-running verification still pulled the agent into coordination noise

`TaskOutput` improved this, but did not eliminate it.

### 2. Review-fix loops are less stable than first-pass implementation

The agent often handled the first implementation pass better than the later
“fix exactly this one comment” phase.

Common failure pattern:

- the review request was narrow
- the agent partially fixed it
- then it launched extra commands or re-expanded the task

This means the repair loop still needs stronger runtime and prompt support than
the first implementation loop.

### 3. Exact workflow obedience still depends too much on supervision

`pre-public runtime` can follow the intended process, but the process is still fragile.

Without tight supervision, it still tends to:

- skip explicit substeps
- reinterpret acceptance criteria
- add exploration that was not requested

In other words, the current system supports supervised execution better than it
supports self-disciplined execution.

### 4. Real GitHub review events work, but real CI events do not

This is not primarily a `pre-public runtime` problem. It is a current `AgentInbox` GitHub
source limitation.

The current source supports:

- PR review/comment style activity
- repo activity feed items

It does not yet support:

- `check_run`
- `check_suite`
- CI-oriented workflow state

So the review loop is real, but the CI loop is still only partially integrated.

## Highest-Signal Product Findings

### Finding 1: Prompt fidelity bugs are worth more than prompt wording tweaks

The `current_input` truncation bug was more damaging than many surface-level
prompt improvements.

Lesson:

- do not assume instruction failure is a model-quality problem
- first verify that the system preserves operator input faithfully

### Finding 2: Capability gaps show up as workflow pathologies

The missing `TaskOutput` tool did not just cause inconvenience. It caused
observable bad behavior:

- extra verification commands
- log-reading workarounds
- command-task fan-out

Lesson:

- when the agent repeatedly invents awkward workflows, the right answer may be
  to add a product primitive, not more guidance

### Finding 3: Long-lived context still needs stronger compaction discipline

Even after the current-input fix, the supervision log still shows that
long-lived agents become harder to steer over time.

The most effective workaround remained:

- one file
- one command
- one success condition

This is useful operationally, but it also signals that context preservation and
compaction still need more product work.

### Finding 4: Worktree isolation should eventually become enforceable

Right now, worktree use works because:

- prompts are narrow
- supervision is staged
- the runtime path has improved

But it is still not a hard invariant.

For high-value supervised development flows, `pre-public runtime` should eventually support
a stronger mode where:

- no managed worktree
- no edits

## Recommended Next Improvements

### Priority 1: Finish tightening the verification loop

The next product win should come from making verification more disciplined.

Recommended direction:

- improve post-task result handling so successful verification terminates cleanly
- reduce the chance of extra verification after a passing result
- keep building on `TaskOutput` rather than forcing the agent back into shell
  workarounds

### Priority 2: Harden supervised repair mode

The repair loop is where the most expensive drift still happens.

Recommended direction:

- improve constrained repair prompting
- make narrow acceptance criteria easier to preserve
- reduce opportunities to reopen scope after a clear repair request

### Priority 3: Make worktree requirements enforceable for selected flows

For supervised dogfooding tasks, a runtime-backed “must use managed worktree”
mode would reduce a lot of operator burden.

Recommended direction:

- selected operator tasks can require active managed worktree
- mutating tools reject edits if the worktree requirement is not satisfied

### Priority 4: Extend `AgentInbox` GitHub support to CI-oriented events

The next real integration milestone is not more fixture coverage. It is real CI
signal ingestion.

Recommended direction:

- extend the GitHub source beyond repo activity feed
- add check-run/check-suite or equivalent workflow-state support

This is tracked in `holon-run/agentinbox#2`.

## Current Assessment

`pre-public runtime` is now good enough to act as a supervised real-project development
agent.

It is not yet good enough to be treated as a low-supervision autonomous
developer for open-ended tasks.

The strongest current fit is:

- real but scoped implementation work
- PR review repair loops
- worktree-isolated changes
- external wake-driven continuation through `AgentInbox`

The weakest current fit is:

- broad open-ended refactors
- loosely specified review follow-ups
- long verification loops without narrow acceptance checks

## Suggested Operating Mode For The Next Round

For the next dogfooding cycle, keep the same overall model:

- real issues
- real PRs
- real GitHub review wake events

But keep these supervision constraints:

- one fresh agent per substantial task
- one worktree per task
- one narrow follow-up objective per repair prompt
- one explicit verification command when possible
- takeover after repeated repair-loop drift, not after unlimited coaching

That mode is currently the best balance between learning value and execution
reliability.
