# pre-public runtime Dogfooding Supervision Log

Date: 2026-04-05
Supervisor: Codex
Target repo: `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation`
Current baseline: `main` at `a02291a`

## Purpose

This log records issues observed while supervising pre-public runtime as it attempts to
develop pre-public runtime itself. The goal is to avoid losing operational findings during
the dogfooding loop.

## Session: Issue #15

Issue:
- `#15 Dogfood: extract a tool guidance registry from the prompt assembly`

Environment:
- release binary rebuilt on `a02291a`
- dedicated data dir: `/tmp/runtime-incubation-dogfood`
- dedicated agent id: `dogfood`
- HTTP addr: `127.0.0.1:7879`

### Observation 1: CLI `prompt` path is currently broken

Time:
- 2026-04-05 00:25 +0800

What happened:
- Running `runtime-incubation prompt --agent dogfood ...` failed immediately.
- The CLI returned:
  - `{"error":"public enqueue only accepts channel or webhook origins","ok":false}`

Likely cause:
- The CLI still posts operator prompts to the public `/agents/:agent_id/enqueue`
  path.
- HTTP ingress rules now reject operator-origin messages on that public path.
- Trusted operator prompt ingress has moved to `/remote/agents/:agent_id/prompt`
  behind `PRE-PUBLIC RUNTIME_REMOTE_TOKEN`, but the CLI has not been updated accordingly.

Impact:
- Local operator use through the built-in CLI is currently broken.
- For this supervision session, the workaround is to use remote prompt ingress
  instead of the CLI.

Suggested follow-up:
- Add a dedicated issue for CLI/HTTP ingress contract drift unless one already
  exists.

### Observation 2: First attempt misclassified the task as follow-up explanation

Time:
- 2026-04-05 00:25 +0800

What happened:
- The first trusted prompt asked pre-public runtime to implement `#15`.
- Instead of starting the work, pre-public runtime produced a response claiming it needed to
  inspect a "Latest completed result brief" and then called `Sleep`.

Evidence summary:
- It talked about a missing completed result brief for issue `#15`.
- No repo inspection, no todo creation, no worktree creation, and no code
  changes happened in that first attempt.

Likely cause:
- Prompt/context assembly is still vulnerable to confusing "current execution
  task" with "follow-up explanation of recent work".
- The presence of recent brief-oriented context appears strong enough to pull
  the model into explanation mode even under a direct operator execution task.

Impact:
- A real implementation task was dropped on the first turn.
- This is a high-signal dogfooding failure for the prompt assembly system.

Workaround used:
- A second operator prompt explicitly stated:
  - there is no completed result brief
  - the previous response was incorrect
  - this is an execution task, not a summary task

Suggested follow-up:
- Fold this into prompt-system work under `#14`.
- Strong candidates:
  - reduce follow-up/brief bias in current prompt assembly
  - make execution-vs-explanation mode boundaries more explicit
  - add a regression fixture for "implement issue" prompts after a brief exists

### Observation 3: Worktree requirement was not obeyed early in the second run

Time:
- 2026-04-05 00:26 +0800

What happened:
- On the second prompt, pre-public runtime correctly began code inspection and created a todo
  list.
- It then started editing `src/tool/spec.rs`.
- At that point there was still no evidence of:
  - `EnterWorktree`
  - a managed worktree session
  - branch creation

Likely cause:
- The agent treated the worktree requirement as secondary compared with "start
  coding now".
- The current prompt/tool guidance may not make sequencing constraints like
  "must enter worktree before editing" sticky enough.

Impact:
- Even when the model starts the real task, it may violate required workflow
  constraints before coding.

Current status:
- Supervision still in progress.
- Need to see whether the run self-corrects later or requires intervention.

Suggested follow-up:
- Add explicit workflow gating for dogfooding tasks when branch/worktree
  isolation is required.
- Consider stronger prompt guidance or runtime-level checks for "must not edit
  main checkout" supervision runs.

### Observation 4: The agent edited the main checkout instead of a managed worktree

Time:
- 2026-04-05 00:28 +0800

What happened:
- The second run progressed far enough to inspect prompt/tool files and start
  implementing the change.
- It edited:
  - `src/prompt.rs`
  - `src/tool/dispatch.rs`
  - `src/tool/spec.rs`
- The main checkout became dirty while `agent.worktree_session` remained `null`.
- `git worktree list` showed no newly created worktree for this attempt.

Impact:
- This violates the explicit supervision contract for dogfooding tasks that
  require isolation.
- It means the run is not acceptable for landing even if the code happened to
  work.

Interpretation:
- The current tool/prompt stack does not reliably preserve sequencing
  constraints like "enter worktree before the first edit".
- The failure is not just missing capability. `EnterWorktree` exists, but the
  workflow instruction was not sticky enough.

### Observation 5: Command verification handling degraded into task fan-out

Time:
- 2026-04-05 00:28 +0800

What happened:
- After the first compile error, the agent fixed the code and re-ran
  verification.
- It then started multiple long-running `exec_command` calls that promoted to
  `command_task`, including:
  - `cargo build --quiet 2>&1`
  - `cargo test --quiet 2>&1`
  - `cargo test prompt::tests --quiet 2>&1 | head -100`
  - a single-test invocation
- The agent reached:
  - `status = awaiting_task`
  - `pending = 6`
  - multiple active task ids

Why this matters:
- The agent did not settle one verification path before spawning the next.
- This is a coordination failure, not just a test-runtime issue.

Likely cause:
- Current guidance for `exec_command` and `TaskGet/TaskList` is not strong enough
  to prevent verification fan-out during active background tasks.
- The model appears to treat each blocked verification step as a cue to launch
  another command instead of coordinating the existing task set.

Suggested follow-up:
- Add a regression scenario for "one verification command becomes a
  command_task; the agent must inspect/wait rather than start more builds/tests".

### Observation 6: Output inspection path is awkward for command tasks

Time:
- 2026-04-05 00:28 +0800

What happened:
- The agent used `TaskGet` and saw an `output_path` under
  `/tmp/runtime-incubation-dogfood/agents/dogfood/task-output/...`.
- It then tried to read that log with the `Read` tool.
- The read failed with:
  - `path escapes active root`

Interpretation:
- This is not necessarily a bug in path enforcement, but it is a workflow
  mismatch.
- `TaskGet` returns an output path that is not readable through normal workspace
  file tools, yet the agent is encouraged to inspect output.

Impact:
- Task output is less usable than it appears from the tool surface.
- The model can get stuck between:
  - "inspect task output"
  - "workspace tools cannot read that path"

Suggested follow-up:
- Either expose task output through a first-class task-output read path, or
  adjust `TaskGet` / prompt guidance so the agent does not assume normal file
  reads will work on out-of-workspace logs.

### Observation 7: Supervision should not run in the user's live checkout

Time:
- 2026-04-05 00:31 +0800

What happened:
- After restoring the invalid attempt's edits, the main checkout still had an
  unrelated modified file:
  - `src/runtime/provider_turn.rs`

Interpretation:
- The user's live checkout may contain in-progress work unrelated to the
  dogfooding session.
- Even if pre-public runtime were behaving correctly, running supervision directly in the
  live checkout creates unnecessary interference risk.

Decision:
- Future dogfooding runs should use a dedicated clean clone or equivalent clean
  supervision workspace instead of the user's active checkout.

### Observation 8: Stage-gated supervision improves behavior, but exact substeps are still skipped

Time:
- 2026-04-05 00:33 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- issue target postponed from `#15` to simpler `#20`

What happened:
- A Stage 0 gate asked the agent to:
  - enter a managed worktree
  - verify isolation with both `GetAgentState` and `exec_command`
  - avoid edits/tests/PRs
- The agent successfully entered a managed worktree:
  - branch: `dogfood-issue-prep`
  - worktree path: `/tmp/.runtime-incubation-worktrees-runtime-incubation-supervisor/dogfood-issue-prep`
- The main checkout remained clean.
- However, it skipped the explicit verification substeps and went directly to
  `Sleep`.

Interpretation:
- Stage gating helps materially: it prevented premature edits and got the agent
  into an isolated workspace.
- But even under a narrow workflow gate, the model still tends to stop after
  satisfying what it sees as the main goal, rather than executing every listed
  substep exactly.

Implication for supervision:
- The workflow should continue using narrow stages.
- But stages should be even tighter and more atomic when exact compliance
  matters.

### Observation 9: Clean supervision clone needs a real GitHub remote for PR creation

Time:
- 2026-04-05 00:23 +0800

What happened:
- In the clean supervision clone, `origin` pointed to a local filesystem path:
  - `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation`
- The agent successfully committed and pushed branch `dogfood-issue-prep`, but
  `gh pr create` failed because no remote pointed at a recognized GitHub host.

Impact:
- The agent could not complete the PR-creation step even though the work itself
  was ready.

Interpretation:
- This is a supervision-environment problem, not primarily an agent reasoning
  failure.

Decision:
- For future clean-clone supervision runs, rewrite `origin` (or add a GitHub
  remote) so that:
  - `git push` targets the actual GitHub repo
  - `gh pr create` can resolve repository metadata normally

### Observation 10: Completion guard can terminate a real doc-fix loop before the first edit

Time:
- 2026-04-05 08:27 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- follow-up on PR `#21`

What happened:
- After review feedback was sent back to the agent, it:
  - read the PR feedback
  - created a todo list
  - removed `ISSUE_20_RECONCILIATION_SUMMARY.md`
  - read the callback doc
  - grepped the problematic expiry lines
- Before it made the first content edit to the callback doc, the runtime
  emitted `completion_guard_triggered` with:
  - `reason = "post_verification_stagnation"`
- The agent then produced a partial brief and went back to sleep.

Interpretation:
- For docs-only review-fix tasks, the current completion guard can misread
  "read + grep + no edit yet" as stagnation after verification.
- This is not primarily a worktree or repo-state problem. It is a runtime
  liveness / stop-condition problem during narrow iterative editing loops.

Implication for supervision:
- Follow-up repair prompts should be split into tighter single-file stages.
- The supervisor should prefer:
  - one concrete file
  - one concrete edit objective
  - no bundled multi-file cleanup
- If this pattern repeats, pre-public runtime itself will likely need a runtime or prompt
  change so completion guard does not fire before the first edit in a review-fix
  loop.

### Observation 11: Literal command-based acceptance criteria work better than descriptive correction requests

Time:
- 2026-04-05 08:46 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- follow-up on PR `#21`

What happened:
- Several rounds of review feedback told the agent, in natural language, to
  remove overclaiming language from
  `docs/callback-capability-and-providerless-ingress.md`.
- The agent partially complied each time, but repeatedly stopped after fixing
  only one sub-problem:
  - first `expiry` wording
  - then one top-level sentence
- Progress only became reliable once the supervisor switched to a literal
  acceptance criterion:
  - `rg -n "implemented and shipped|Implementation status: All shipped|✓" docs/callback-capability-and-providerless-ingress.md`
  - expected result: no matches

Interpretation:
- For narrow cleanup tasks, pre-public runtime responds better to an externally checkable
  success predicate than to prose about tone or intent.
- This is especially true when a document contains many repetitive surface
  issues.

Implication for supervision:
- Future review-fix loops should prefer:
  - a concrete verification command
  - a required exit condition
  - minimal freedom to reinterpret “done”
- Longer-term product implication:
  - pre-public runtime could benefit from a built-in “operator-supplied acceptance check”
    pattern for narrow repair tasks.

### Observation 12: Externally deleting a managed worktree leaves the running agent in a stale worktree session

Time:
- 2026-04-05 08:49 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- transition from issue `#20` to issue `#16`

What happened:
- After issue `#20` landed, the supervisor cleaned up the old Git worktree on
  disk:
  - removed `/tmp/.runtime-incubation-worktrees-runtime-incubation-supervisor/dogfood-issue-prep`
- But the running agent still had:
  - `worktree_session.worktree_path = /tmp/.runtime-incubation-worktrees-runtime-incubation-supervisor/dogfood-issue-prep`
- On the next task, `ExitWorktree` failed because the path no longer existed,
  and file/system tools started failing with path-root errors such as:
  - `managed worktree path does not exist`
  - `path escapes active root`

Interpretation:
- Current runtime recovery clears missing worktrees on startup, but the
  long-lived running agent does not self-heal if the managed worktree is
  removed externally while the process stays alive.
- This is an environment/state-coherency problem, not primarily a reasoning
  failure by the agent.

Implication for supervision:
- Do not remove a managed worktree behind a live agent without first clearing
  or exiting the worktree in-band.
- If external cleanup already happened, the supervisor should repair the
  environment by clearing the stale `worktree_session` and restarting the
  runtime before assigning the next task.

### Observation 13: The supervised release binary must come from a clean, known revision

Time:
- 2026-04-05 08:55 +0800

Environment:
- source checkout: `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation`
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- running release binary: `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/target/release/runtime-incubation`

What happened:
- During issue `#16` supervision, the supervisor re-checked whether the running
  `runtime-incubation` binary actually matched the latest repository state.
- The currently running binary had been built earlier than the newest shipped
  changes under supervision.
- The primary source checkout also had unrelated local modifications in
  `src/runtime/provider_turn.rs`, which made it a poor source of truth for a
  “latest release” rebuild.
- The clean clone used for supervision had not yet fetched the latest remote
  state, so there was no guaranteed clean reference until the supervisor
  refreshed it explicitly.

Interpretation:
- A long-running supervision session can silently drift away from the code
  revision it is supposed to evaluate.
- “Use target/release/runtime-incubation” is not a sufficient contract unless the supervisor
  also records:
  - which checkout produced it
  - whether that checkout was clean
  - whether it matched the latest remote revision

Implication for supervision:
- Before assigning a new dogfooding issue, the supervisor should ensure the
  runtime binary is built from:
  - a clean checkout
  - a fetched latest `origin/main`
  - a recorded revision
- In practice, the clean supervision clone is the safest build source for the
  dogfooding runtime.

### Observation 14: Long-lived agent context causes operator prompts to truncate in the middle of actionable instructions

Time:
- 2026-04-05 09:52 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- issue: `#16`

What happened:
- After PR `#22` was opened, the supervisor tried to drive a few review-fix
  rounds by sending normal operator prompts with 2-3 bullets of feedback.
- Even prompts that were not especially long from the supervisor's perspective
  were repeatedly truncated in the model-visible context:
  - one prompt cut off at `demonstrate both enqueue_...`
  - another at `attempt one more callback d...`
  - another at `Also change moc...`
- The agent reacted reasonably: it refused to guess and asked for the complete
  instruction. But this made multi-part repair loops stall.

Interpretation:
- The dominant issue is not raw instruction quality; it is accumulated context
  bloat inside the long-lived agent.
- Once the running transcript/history becomes large enough, even moderately
  sized operator prompts lose their tail.
- This is especially visible late in a supervision session after many staged
  prompts, PR loops, and internal follow-ups.

Implication for supervision:
- Late-session review fixes should assume severe prompt truncation risk.
- The supervisor should either:
  - restart with a fresh agent for the next substantial task, or
  - reduce each operator prompt to a single actionable objective.
- Longer-term product implication:
  - pre-public runtime needs a better compaction / operator-message preservation strategy so
    the tail of operator instructions remains intact.

### Observation 15: Single-goal prompts are the most reliable workaround once truncation starts

Time:
- 2026-04-05 09:54 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood20`
- issue: `#16`

What happened:
- Multi-part follow-up prompts for PR `#22` repeatedly failed because the tail
  of the instruction was truncated.
- Once the supervisor switched to single-goal prompts, progress resumed:
  - one prompt only for `test.js`
  - one prompt only for `README.md`
  - one prompt only for `git push`
- Each of these narrowly scoped prompts was executed successfully, and the PR
  eventually landed.

Interpretation:
- When the agent is already context-heavy, the safest operator protocol is:
  - one file or one action
  - one success criterion
  - no bundled cleanups
- This is more reliable than trying to preserve “efficiency” with one broader
  repair prompt.

Implication for supervision:
- For future dogfooding tasks, especially after the first PR review round, the
  supervisor should default to:
  - one file or one command
  - one observable success condition
  - then another prompt for the next unit of work

### Observation 16: "Planning only" does not reliably prevent file writes

Time:
- 2026-04-05 10:03 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17`
- issue: `#17`

What happened:
- The operator prompt explicitly said:
  - planning only
  - inspect implementation and tests
  - do not edit files yet
- The agent still created two planning documents in the managed worktree:
  - `docs/issue-17-recovery-hardening-analysis.md`
  - `docs/issue-17-quick-reference.md`
- It then reported the stage as successfully completed.

Interpretation:
- The current behavior treats “document the analysis” as compatible with
  planning, even when the operator intends planning to stay ephemeral.
- A plain-language "do not edit files yet" instruction is not sticky enough to
  block low-risk writes like notes or planning docs.

Implication for supervision:
- For planning-only stages, the operator should assume that any writable tool
  may still be used unless the task is reduced further.
- Better supervision protocol:
  - either avoid planning-only stages entirely
  - or constrain the stage to a read-only success condition with an explicit
    final response format and no planning artifact files

### Observation 17: Edit and ApplyPatch failures still derail otherwise-correct implementation attempts

Time:
- 2026-04-05 10:19 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17`
- issue: `#17`

What happened:
- After the supervisor narrowed the task to one recovery slice, the agent found
  the correct implementation point and successfully changed `src/runtime/tasks.rs`.
- It then got stuck trying to add one targeted test:
  - `ApplyPatch` failed first because the payload did not use the required
    `*** Begin Patch` grammar
  - a later `ApplyPatch` attempt used diff-style headers like
    `--- a/src/runtime/tasks.rs`, which the tool rejected
  - repeated `Edit` attempts failed because `old_string` was omitted
- At this point the agent was no longer blocked on design; it was blocked on
  tool invocation discipline.

Interpretation:
- The current agent can often identify the right code path, but the editing
  primitive contract is still not sticky enough under iteration.
- This remains a major bottleneck for dogfooding tasks that require even small
  follow-up test edits after the first code change lands.

Implication for supervision:
- When the failure mode is clearly tool-schema related, the supervisor should:
  - avoid taking over the code change immediately
  - send one narrow correction that focuses only on the remaining edit
  - explicitly remind the agent of the required tool-call shape
- Longer term, pre-public runtime still needs stronger in-prompt or runtime-visible guidance
  for `Edit` and `ApplyPatch` usage.

### Observation 18: Shell pipelines in verification hide real command failures

Time:
- 2026-04-05 10:22 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17`
- issue: `#17`

What happened:
- The agent repeatedly used verification commands like:
  - `cargo test ... 2>&1 | head -50`
  - `cargo check --lib 2>&1 | tail -20`
- These commands surfaced useful output, but the pipeline exit status stayed `0`
  even when Rust compilation failed.
- The agent then treated the verification as successful and kept moving.

Interpretation:
- For dogfooding and supervision, shell pipelines are currently unsafe as the
  primary verification command unless `pipefail` is set explicitly.
- This creates false positives exactly when the agent most needs reliable
  feedback.

Implication for supervision:
- Verification prompts should prefer:
  - direct `cargo test ...`
  - direct `cargo check ...`
  - or explicit `set -o pipefail`
- A future pre-public runtime improvement should make shell verification failures harder to
  misread when output is piped through `head` or `tail`.

### Observation 19: Task-status and task-result re-entry can destabilize focused repair loops

Time:
- 2026-04-05 10:22 +0800

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17`
- issue: `#17`

What happened:
- During one narrow repair step, the agent spawned multiple background command
  tasks to wait on other command tasks and inspect their output.
- Those task status/result messages re-entered the same agent while it was still
  trying to finish the original code repair.
- The result was a noisy loop:
  - more pending messages
  - more active task ids
  - less focus on the actual fix

Interpretation:
- The current runtime is vulnerable to “verification self-amplification”:
  task progress messages can easily become new work rather than just context.

Implication for supervision:
- When a repair loop starts drifting into self-observation, the safer move is:
  - stop that agent iteration
  - restart from a fresher context
  - use one direct verification command instead of task-chasing follow-ups

### Observation 20: Even file-scoped prompts still drift into repo inspection and the wrong verification command

Time:
- 2026-04-05 10:41 CST

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17s`
- issue: `#17`

What happened:
- The supervisor sent a much narrower implementation prompt:
  - only inspect and edit `src/runtime.rs` and `src/types.rs`
  - do not search for issue text
  - do not use piped verification commands
  - run exactly `cargo test runtime_clears_missing_worktree_session_during_recovery -- --nocapture`
- The agent did complete stage 0 worktree setup correctly.
- But in the actual implementation stage it still drifted:
  - it ran extra repository inspection commands
  - it launched `cargo test 2>&1 | head -100`
  - it then treated that background command task as the next unit of work
- At the point of inspection, the worktree was still clean and no code change had
  been made.

Interpretation:
- Tightening the prompt to a file scope is not enough by itself.
- The agent still defaults to “inspect and verify first” behavior, even when the
  supervisor has already specified the exact expected change and exact test.
- This is now a distinct failure mode from generic planning drift: it is
  execution-order drift under a narrow coding brief.

Implication for supervision:
- For the next restart, the supervisor should tighten the loop further:
  - specify the exact behavior change before any verification command
  - forbid any command execution before the first edit is made
  - name the single verification command and make it the only allowed command
- Longer term, pre-public runtime may need better mode guidance for “repair this exact spot”
  tasks so it does not automatically expand back into repository exploration.

### Observation 21: The supervisor can accidentally assign an already-shipped subtask and create fake drift

Time:
- 2026-04-05 10:50 CST

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17s`
- issue: `#17`

What happened:
- The supervisor narrowed `#17` into a smaller task:
  - clear missing persisted worktree session state during recovery
  - emit an audit event
  - add a restart-oriented test
- After inspecting the clean clone, the recovery behavior itself was already in
  `main`:
  - `src/runtime.rs` already cleared a missing `worktree_session`
  - `src/runtime.rs` already appended
    `recovery_cleared_missing_worktree_session`
- The agent still drifted into wrong verification behavior, but part of the
  confusion came from being asked to “implement” something that was no longer an
  open code gap.

Interpretation:
- Not all apparent drift is an agent-only failure.
- When the supervisor assigns a stale subtask, the agent is pushed toward
  unnecessary exploration and fake “nothing changed” loops.

Implication for supervision:
- Before restarting a failed dogfooding thread, the supervisor should re-check
  whether the narrowed subtask is still missing on current `main`.
- For `#17`, the remaining real gaps should be pulled from acceptance criteria
  that are still genuinely unimplemented, such as:
  - callback resolution after restart
  - explicit command-task restart coverage

### Observation 22: Review-phase correction still falls back into verification self-amplification

Time:
- 2026-04-05 11:05 CST

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17u`
- issue: `#17`

What happened:
- The agent produced a local commit for a new callback restart test, but the
  test had two blockers:
  - it did not actually restart host/runtime
  - it did not compile
- The supervisor then sent a narrow review-style correction prompt with
  explicit blockers and one exact verification command.
- Even in this review phase, the agent drifted back into the same pattern:
  - spawned multiple background verification tasks
  - used piped cargo commands again
  - chased task output files with more helper commands
  - kept re-entering through task status/result messages

Interpretation:
- The current failure mode is not limited to greenfield implementation prompts.
- Even when the target diff already exists and the next step is “fix these two
  concrete blockers”, the agent still tends to expand into self-verification
  loops.

Implication for supervision:
- After one failed review-fix loop, the supervisor should strongly prefer:
  - pausing that agent/worktree
  - carrying forward the concrete review findings
  - restarting from a fresh agent with a smaller, more explicit patch brief
- Longer term, pre-public runtime likely needs stronger built-in mode guidance for
  “review fix” phases so they do not devolve into background task orchestration.

### Observation 23: Background command-task churn leaves agent state noisy even after pausing

Time:
- 2026-04-05 11:05 CST

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agent: `dogfood17u`
- issue: `#17`

What happened:
- During the failed verification loop, the agent created many background command
  tasks (`cargo`, `sleep`, log-tail helpers, and status checks).
- After the supervisor paused the agent, agent status still reported:
  - `status = awaiting_task`
  - many `active_task_ids`
  - a nonzero `pending` count

Interpretation:
- Once a repair loop has been contaminated by background verification churn, the
  runtime state becomes a poor substrate for continued focused work.
- Even if the human supervisor conceptually wants to “pause and resume”, the
  leftover task state keeps the agent in a noisy intermediate mode.

Implication for supervision:
- For dogfooding, a fresh agent/worktree is usually safer than trying to resume
  an agent after a background-task-heavy failure.
- This also suggests pre-public runtime could benefit from a clearer “stop verification
  tasks and quiesce” operation for supervision-heavy workflows.

### Observation 24: After repeated drift, the supervisor eventually needs a takeover threshold

Time:
- 2026-04-05 11:20 CST

Environment:
- clean supervision clone: `/tmp/runtime-incubation-supervisor`
- agents: `dogfood17u`, `dogfood17v`
- issue: `#17`

What happened:
- Multiple attempts could reach parts of the right area:
  - use a managed worktree
  - edit the right test file
  - approximate the desired callback-restart scenario
- But they repeatedly failed to fully close the task:
  - wrong test names
  - pseudo-restart instead of real host/server restart
  - piped verification instead of the exact required command
  - self-amplifying background verification loops
- At that point the supervisor took over the patch directly and finished the
  test in the same worktree.

Interpretation:
- There is a practical threshold where continuing to “coach” costs more than a
  direct human patch, even if the agent is nearby semantically.
- The hard part here was not understanding the feature. It was reliable
  execution discipline under narrow review constraints.

Implication for supervision:
- Dogfooding should keep a clear takeover threshold such as:
  - two fresh attempts
  - or one failed implementation plus one failed review-fix cycle
- Once that threshold is crossed, the supervisor should:
  - finish the task directly
  - record the precise failure mode
  - feed those learnings back into the next dogfooding task design
