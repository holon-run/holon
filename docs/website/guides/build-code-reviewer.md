---
title: Build a local Code Reviewer that follows PRs
summary: Create a reviewer agent from the code-reviewer template, then ask it in conversation to attach your repository, record merge permissions, subscribe to GitHub events, and follow commits and CI through merge.
order: 88
---

# Build a local Code Reviewer that follows PRs

This guide shows how to set up a long-lived GitHub Pull Request reviewer from the `code-reviewer` template. After creating the agent in the Web GUI, you do most of the setup in plain conversation: ask the agent to attach the repository, record its review and merge boundaries, connect event subscriptions, and re-verify PRs as new commits and CI results arrive.

- For a one-shot command-line review of a single PR, see [Run a GitHub task with `holon solve`](./run-github-task.md).
- For the real production case behind this workflow (including how `holon-reviewer` handled multi-round fixes on PR #2854 and reviewed 95% of merged PRs over three months), read [Beyond a one-time review: build a reviewer that follows PRs with Holon](../blog/one-pr-one-work-item.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- The machine running Holon has the GitHub CLI (`gh`) authenticated and the target project's build and test toolchain installed.
- No need to memorize CLI flags or hand-edit config files: cloning the repository, attaching the workspace, updating role rules, and wiring up event subscriptions can all be delegated to the agent in conversation.

## Steps

### 1. Create the agent from the `code-reviewer` template

The `code-reviewer` template initializes the agent's role contract (`AGENTS.md`) and installs the required skills:
- `code-review` and `github-review`: evidence-backed finding severity, deduplication against existing threads, and single-surface publishing (never posting both a top-level comment and a formal review in the same round);
- `ghx` and `sview`: GitHub PR context retrieval and structured local source inspection;
- `agentinbox` and `uxc`: repository PR discovery and per-PR event subscriptions.

#### Recommended: Create in the Web GUI

1. Open the Holon Web GUI and click **Templates** (`/templates`) in the left navigation.
2. If `code-reviewer` is not listed yet, click **Install Template** and enter:
   ```text
   https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
   ```
3. Find `code-reviewer` in the template list and click **Create Agent**:
   - **Agent ID**: `reviewer` (or another unused ID such as `repo-reviewer`)
   - **Name**: `Code Reviewer`
4. Confirm creation and open the agent's conversation view.

> **Optional (CLI)**: If you prefer the terminal, run `holon agent create reviewer --template code-reviewer`. See [Create an agent from a template](./create-agent.md) for details.

### 2. Ask the agent to attach the repository and record permissions

The `code-reviewer` template includes a `Permission Confirmation Protocol`: before starting continuous review work, the agent confirms whether it may approve PRs, whether it may merge PRs, whether it should subscribe to events via `agentinbox`, and whether it may push fixes on an author's behalf.

You do not need to edit `AGENTS.md` by hand or run workspace commands yourself. Tell the agent your repository and team rules directly in the chat:

```text
Take responsibility for PR reviews in <owner/repo>:
1. If the repository is not cloned locally yet, clone it and attach it as your workspace (or attach the existing checkout at <absolute path>).
2. Verify changes against local source code and tests rather than guessing from the diff alone. Read the repository's own AGENTS.md first for build and test rules.
3. You may run tests in an isolated worktree, post review comments, and approve PRs.
4. When all required CI checks pass on the final head, no blocking findings remain, and repository merge rules are satisfied, you may squash-merge directly (or change this to: "notify me when merge conditions are met so I can merge").
5. Do not push code fixes to the author's branch; ask me first about large refactors, breaking API changes, or security-sensitive changes.
Please record these standing rules in your AGENTS.md and confirm with me.
```

**What the agent does automatically**:
- Clones the repository if needed and attaches it as a workspace binding;
- Reads the repository's own `AGENTS.md` or contributor guidelines;
- Saves your confirmed merge gates and escalation rules into its own `~/.holon/agents/reviewer/AGENTS.md` so future PRs follow the same policy.

### 3. Trial a single PR review in conversation

Before enabling automatic repository-wide subscriptions, test the workflow on one familiar PR to confirm local test execution and review quality. Send a message in chat:

```text
Please review https://github.com/owner/repo/pull/123 using our standing rules. Check the affected code and tests locally, and publish one structured review without duplicate comment spam.
```

**What the agent does automatically**:
1. Creates a dedicated WorkItem for `PR #123` anchored to the current `head SHA`;
2. Inspects the PR diff and existing review threads, checks out the code in an isolated worktree, and runs relevant builds or tests;
3. Separates blocking findings from non-blocking suggestions with file/line citations and test evidence, then publishes a single GitHub Review;
4. Reports a structured summary in the chat and WorkItem:

```text
Reviewed version: <head SHA>
Blocking findings: <finding, location, and evidence>; or no blocking findings found in this review
Verification: <tests run and results>; <areas not covered>
Next step: wait for <author's fix / CI for the current head / maintainer's decision>
```

### 4. Ask the agent to connect repository and PR event subscriptions

Once you are satisfied with the trial review, ask the agent to wire up automatic PR discovery and follow-up.

Instead of running subscription CLI commands yourself, instruct the agent in chat:

```text
Use your agentinbox Skill to connect new-PR discovery subscriptions for <owner/repo>, plus per-PR follow-up for new commits, CI updates, and review comments. Tell me if any tool or authentication step needs my action.
```

**What the agent does automatically**:
- Follows its `agentinbox` and `uxc` skills to check the local service, repository access, and its own wake target;
- If a tool is missing or needs authentication, walks you through the login flow step by step (never paste raw tokens into the chat). For background on the event bridge, see the [AgentInbox onboarding guide](https://agentinbox.holon.run/guides/onboarding-with-agent-skill);
- Sets up two subscription layers:
  - **Long-lived repository subscription**: watches for newly opened PRs, creates a WorkItem for each, and starts the initial review;
  - **Per-PR follow-up subscription**: watches active PRs for new pushes, CI completions, and review replies, then completes the WorkItem and cleans up that PR's subscription once merged or closed.

> **Tip**: A new event subscription only receives future events. If you also want the reviewer to pick up PRs that are already open right now, add: `"Also take on the currently open PRs in this repository."`

### 5. Verify automatic re-review and merge cleanup

Let a real PR go through an update cycle without sending `"please continue"` messages in between:

1. **Automatic PR discovery**: When a new PR opens, confirm in the Web GUI's WorkItems view that `reviewer` creates a WorkItem and posts the initial review.
2. **Automatic re-review on new commits or CI**: When the author pushes a fix or CI finishes, the event wakes the same WorkItem. `reviewer` checks the new `head SHA`, verifies whether previous blocking findings are resolved, checks CI for that exact commit, and flags any new regressions—**without re-posting resolved comments**.
3. **Merge or escalation**:
   - When required checks on the final `head` pass and no blockers remain, `reviewer` merges the PR (or notifies you, depending on your policy), marks the WorkItem completed, and removes the per-PR subscription;
   - If a change crosses into architecture or security escalation boundaries, it pauses and asks for your decision.

## Troubleshooting

When something looks off, ask `reviewer` to diagnose it in conversation first:

- **A new PR opened, but review did not start automatically**:
  Ask in chat: `"Check your AgentInbox repository subscription for <owner/repo> and verify whether your wake target is active."`
- **The author pushed a new commit, but the agent cited old CI results**:
  Ask in chat: `"Re-check the latest head SHA on this PR and verify the CI status for that specific commit."`
- **You want to tighten review rules or switch to manual merging**:
  Tell it in chat: `"Stop merging PRs automatically. When merge conditions are met, approve the PR and notify me instead. Update your AGENTS.md."`

## See also

- [Beyond a one-time review: build a reviewer that follows PRs with Holon](../blog/one-pr-one-work-item.md): The production story and WorkItem design behind how `holon-reviewer` reviewed 763 merged PRs over three months.
- [Run a GitHub task with `holon solve`](./run-github-task.md): Run a one-shot CLI task against a single GitHub issue or PR.
- [Connect external events and webhooks](./connect-external-events.md): How external event ingress, wake callbacks, and trust boundaries work.
- [Work in a project workspace or worktree](./work-with-workspaces.md): How workspace bindings and isolated worktrees keep local checks clean.
- [Create an agent from a template](./create-agent.md): Template installation and agent initialization details.
