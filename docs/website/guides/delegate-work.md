---
title: "Delegate Complex Tasks to Subagents Safely and Verify Deliverables"
summary: Instruct your primary agent in plain language to delegate deep audits, isolated test runs, or refactors to scoped subagents with strict read-only or worktree boundaries, then verify deliverables and clean up instances safely.
order: 80
---

# Delegate Complex Tasks to Subagents Safely and Verify Deliverables

In complex projects, forcing a single agent to handle every task serially bloats its context window. Long-running test suites, deep security audits, and multi-file refactors can also stall your interactive conversation.

In Holon, you can instruct your primary agent in plain language—via the Web GUI or terminal—to spin up supervised subagents with specialized roles (such as a code reviewer or QA engineer). The parent agent establishes clear execution boundaries (either an inherited read-only workspace or an isolated Git worktree), suspends itself with zero token overhead during child execution, audits all deliverables, and safely tears down transient instances.

The parent agent retains full responsibility for acceptance and final delivery. Subagent findings serve as evidence, never as unverified commands.

## Prerequisites

- A running Holon runtime and [Web GUI](./use-web-gui.md) (default `http://localhost:8080`) or CLI.
- An active primary agent (such as `main` or `developer`).
- A target repository or workspace attached to the agent (see [Work with Workspaces and Git Worktrees](./work-with-workspaces.md)).
- Available agent templates (such as `code-reviewer`, `qa-engineer`, or `software-developer`; see [Organize Agent Skills and Roles](./organize-agent-skills-and-roles.md)).
- No manual JSON drafting: you instruct the parent agent conversationally, and the agent coordinates tool calls and evidence collection.

## Steps

### 1. Identify Delegation Opportunities and Select Workspace Isolation

Delegation improves **concurrency, context hygiene, and workspace safety**. Apply it purposefully:

- **When to delegate**:
  - **Read-only code audits**: Auditing uncommitted diffs or specific commits for security, performance, or edge cases without disrupting the parent's current edit context.
  - **Time-consuming test runs**: Executing end-to-end integration suites, benchmarks, or long builds.
  - **Exploratory or high-risk edits**: Prototyping speculative refactors that must prove viable before touching your working branch.
- **When not to delegate**:
  - Editing two configuration lines, looking up a function definition, or answering direct questions. The parent agent handles these directly with lower latency and zero orchestration overhead.

Choose the appropriate workspace isolation mode based on the task:

| Workspace Mode | Best For | Isolation & Safety |
| --- | --- | --- |
| **Inherit (`inherit`)** | Read-only audits, index generation, documentation validation | Shares the parent workspace view. **Must enforce strict read-only behavior**: child agents cannot edit files, eliminating write conflicts. |
| **Worktree (`worktree`)** | Speculative refactors, feature branches, test artifact generation | Checks out a separate Git branch in an isolated worktree directory. Compiles, edits, and scratch files remain isolated from the parent workspace. |

### 2. Instruct Delegation in Natural Language

You do not need to assemble raw `InvokeAgent` JSON objects. Tell your parent agent what role, target scope, boundary constraints, and deliverable format to apply.

#### Scenario A: Read-Only Code Review (Inherited Workspace)

> "Spin up a `code-reviewer` subagent to audit our recent changes under `src/runtime/`.  
> **Constraints:** Strict read-only mode—do not edit, commit, or push any files. Focus on edge-case handling and concurrency regressions. List each issue with exact file paths, line numbers, severity, and rationale. If no bugs are found, list the exact files reviewed."

#### Scenario B: Speculative Refactoring & Verification (Worktree Isolation)

> "Delegate a `software-developer` subagent in a fresh Git worktree to refactor token expiration handling in `src/auth/token.rs`.  
> **Constraints:** Run unit tests to confirm green status. Once tests pass, commit the changes to the new branch and report the diff and test summary. Do not merge into main; wait for my review."

Upon receiving your prompt, the parent agent executes the following steps:
1. Calls `InvokeAgent` with the appropriate template and workspace isolation mode.
2. Registers a task-result wait (`wake=task_result`) and yields control, consuming zero tokens while the subagent runs.
3. Tracks subagent execution in the Web GUI sidebar.

### 3. Verify Subagent Deliverables

When the subagent completes, the runtime wakes the parent agent. Before presenting conclusions or accepting code, the parent agent audits the deliverables:

| Verification Target | Acceptance Criteria & Pitfall Defense |
| --- | --- |
| **Revision Alignment** | Confirm the subagent inspected or edited the requested commit SHA or branch, avoiding stale worktree revisions. |
| **Concrete Evidence** | Each reported issue must cite valid file paths, exact line numbers, and causal reasoning rather than speculative generalities. |
| **Negative Confirmation** | If the subagent reports "no issues found", it must document the exact files and test cases evaluated to prevent false-green passes. |
| **Test Authenticity** | For code tasks, verify exit codes and output logs from actual test runs, confirming assertions were evaluated. |
| **Authority Boundaries** | Subagents cannot authorize branch merges or production releases. Authority remains with the parent agent and operator. |

### 4. Synthesize the Brief and Safely Clean Up Instances

After auditing the subagent's deliverables, the parent agent presents a structured summary including identified defects, proposed diffs, or test validation receipts.

Once the task is closed, clean up temporary resources:

1. **Reuse or Retain**: Newly created subagents are private, reusable instances retained on disk. If you need follow-up analysis, prompt the parent agent:
   > "Ask the review subagent to double-check error handling in `src/runtime/worker.rs`."
2. **Safe Deletion**: When you are finished, prompt the parent agent to release the subagent:
   > "The review is complete. Safely clean up the subagent and its temporary worktree."
   - **Parent Ownership**: Holon enforces explicit ownership. Only the delegating parent agent can delete its supervised subagents.
   - **Blocker Checks**: Deletion aborts safely if the subagent has active runs, uncommitted changes, or a locked worktree, warning the parent to resolve open dependencies first.
   - **Idle Retirement**: Unused subagent processes automatically retire from memory after an idle timeout while retaining their persistent records on disk.

## Best Practices

- **Do not assume uncommitted edits appear in a new worktree**:  
  When running in `worktree` mode, the child checks out a clean Git branch. It does not see dirty files in your working directory. Commit changes first or provide a patch explicitly.
- **Distinguish Subagents from Peer Agents**:  
  - **Subagents**: Spawned for bounded, supervised tasks; owned and recycled by the parent.
  - **Peer Agents**: Long-lived, independent agents (such as `docs-steward` or `code-health-steward`). Communication occurs via asynchronous messages (`SendAgentMessage`) rather than hierarchical supervision.
- **Avoid Over-Concurrency**:  
  Do not launch multiple subagents competing for write access on the same directory. Use worktrees for code edits and read-only inheritance for reviews.

## Troubleshooting

- **Subagent reports permission denied or missing tool**:  
  Verify the subagent's template contains required skills. A read-only reviewer template deliberately lacks file mutation tools and will reject requests to edit code.
- **Parent agent has not reported back**:  
  The subagent may be downloading dependencies or running a long test suite. Ask the parent agent: "What is the status of the delegated subagent?" The parent will inspect the active task snapshot and report progress.

---

## See Also

- [Work with Workspaces and Git Worktrees](./work-with-workspaces.md)
- [Observe, Track, and Accept Agent Work](./observe-agent-work.md)
- [Organize Agent Skills and Roles](./organize-agent-skills-and-roles.md)
- [Multi-Agent Collaboration Concepts](/concepts/multi-agent-collaboration.md)
