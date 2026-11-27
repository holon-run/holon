# Multi-Agent Delegation, Observation, and Cleanup

## When to Delegate

Default to completing work yourself. Creating or invoking another agent is never a mandatory ceremony; do not delegate a small serial task to an equally capable agent just to wait and relay its output.

Delegate only for a concrete benefit:

- **Real parallelism**: the child owns an independent deliverable (such as non-conflicting modules) while you advance separate useful work.
- **Specialist role or execution context**: distinct review responsibilities, tools, or environment are needed. If only a skill guide is missing, read `SKILL.md` yourself. Prefer an existing authorized specialist; when creating one is warranted, choose the fitting template (`holon template catalog`). Templates never grant authority.
- **Significant context isolation**: a bounded large-log or cross-repository investigation returns conclusions and evidence without flooding the parent context with raw material.
- **Independent review**: specify the exact review question, independent perspective, and time or token budget rather than asking a second agent to re-explore the whole task. Agent review is not human approval.

## Handoff & Resource Serialization

- Before delegating, record the concrete benefit (do not claim a speedup when the benefit is isolation or review), bounded deliverable and acceptance criteria, known evidence, unresolved questions, and what you will advance in parallel.
- Identify shared-file, build/Cargo, or worktree conflicts up front and assign exclusive ownership or serialize access.
- Reuse handed-off evidence; re-read only for concrete gaps, contradictions, state changes, or scoped independent review. The delegating agent remains responsible for integration, verification, follow-through, and supervised-child cleanup.

## Bounded Progress Observation (`holon tail`)

Keep collaboration low-noise on both sides: send unsolicited messages only for final delivery, actionable blockers, contract changes, or ready dependencies.

When a parent needs current child progress and `ExecCommand` is available, inspect read-only progress instead of messaging "how is it going?":

- Bounded snapshot: `holon tail --agent <agent_id> --include-progress --limit 20 --max-events 100 --max-bytes 65536`
- Deadline-bounded follow: `holon tail --agent <agent_id> --include-progress --follow --timeout-ms 5000 --limit 20 --max-events 100 --max-bytes 65536` (never run an unbounded `tail -f`).

`holon tail` is read-only agent-scoped observation: it does not claim or consume task results, wake the target, satisfy reply obligations, or prove task completion, and it may include other work by that agent. Use `TaskStatus` for exact task lifecycle state and `WaitFor` (`wake=task_result`, `resource=<task_id>`) for ordinary task settlement without polling.

## Child Visibility & Safe Deletion

- **Authoritative child roster**: the runtime exposes direct children—including private supervised children—in `AgentSummary.active_children` (`GetAgent` / `ListSubagents`). Use `GetAgent` first rather than maintaining a manual child list.
- **Public roster filter**: `holon agent list --parent self` lists only public named children and omits private supervised children.
- **Preferred safe deletion**: when a disposable child you created is confirmed no longer needed, prefer the native `DeleteAgent(agent_id, incarnation)` tool using a fresh identity from `GetAgent` or `ListSubagents`. Its safe policy preserves user deliverables and in-flight work; the end of one invocation or task does not by itself make a child deletable.
- **Explicit destructive CLI fallback**: only when `DeleteAgent` is unavailable and the operator explicitly intends destructive cleanup, run `holon agent delete <AGENT_ID> --yes --wait` (add `--archive` to export private history/AgentHome first, and `--cascade-private-children` only when intentionally removing private descendants). Never delete shared or long-lived agents merely because their current task is terminal.
