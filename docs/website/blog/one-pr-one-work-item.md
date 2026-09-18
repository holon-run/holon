---
title: "Beyond a one-time review: build a reviewer that follows PRs with Holon"
summary: "Create a reviewer with built-in working rules and Skills, set its responsibilities and merge permissions, and have it discover PRs and follow fixes and CI updates."
order: 30
---

# Beyond a one-time review: build a reviewer that follows PRs with Holon

![A blue reviewer faces a code panel, with a PR workflow in front showing changes, waiting, and checks passing in sequence.](/assets/continuous-pr-reviewer-cover.webp)

> This case comes from the Holon repository's own `holon-reviewer`; case records were collected on September 10, 2026.

The author pushed a fix. The reviewer confirmed that the original problem was resolved. A few minutes later, CI failed again.

That happened in PR #2854 in the Holon repository. It changed work-item completion behavior: when an agent completes another work item, it should not also end the work it is currently executing. The first fix resolved the scheduling problem but introduced an incorrect test expectation; the reviewer raised another blocking finding. Only after the author corrected it and checks on the final version passed did the PR move to merging and cleanup.

| Code version | Review judgment | Next action |
| --- | --- | --- |
| `589405cc` | Two scheduling end-to-end tests failed; traced to incorrect work-item classification | Raise a blocking finding and wait for a fix |
| `19f390ca` | Original problem fixed and scheduling checks passed; another test expectation was reversed incorrectly | Raise another blocking finding and wait for a new version |
| `f7b93809` | Test expectation restored; relevant checks on the final version passed | Confirm the merge result and complete the work item |

This process took place on September 9, 2026. Commits, public reviews, and the merge result have been checked against GitHub snapshots. Some CI jobs were conditionally skipped; “passed” in the table refers only to the relevant checks that actually ran.

Across the three months from June 12 to September 12, 2026, Holon Reviewer reviewed **763 merged PRs** in the Holon repository—**about 95%** of the 804 PRs merged during that period—and submitted **1,529 reviews** on those PRs.

These figures combine reviews submitted by `holonbot` and `jolestar`, the personal GitHub account used by the local Reviewer, and exclude Copilot. PR coverage counts each PR once.

The steps below set up a reviewer to pick up new repository PRs automatically and follow them through fixes and CI updates.

## Create a reviewer from a template

First follow [Create your first agent](../getting-started/first-agent.md) to install Holon, configure a model, and start the long-running runtime. The machine running Holon needs access to the target repository, the GitHub CLI (`gh`), and the project's testing tools. Receiving events continuously also uses AgentInbox and UXC; we will check the integration later.

In your own terminal, create an agent named `reviewer` from the official template:

```bash
holon agent create reviewer --template https://github.com/holon-run/holon/tree/4895eafce1926cb4cc4687c5b9a99c13b071a1e4/agent_templates/code-reviewer
```

This example pins the template revision so the command does not depend on when the rename from `holon-reviewer` to `code-reviewer` reaches `main`. If you have already installed or synced the renamed template, you can also use its name. Choose either method:

```bash
holon agent create reviewer --template code-reviewer
```

`reviewer` is the name of the agent you create; `code-reviewer` is the template name. If an agent with that name already exists, use another name rather than overwrite its work records.

After creation, run `holon agent list` to confirm that the new agent appears, then select it from the TUI or Web GUI.

### The template's built-in AGENTS.md

The `code-reviewer` template includes `AGENTS.md`, which supplies the new agent's initial working rules. Creation also installs the Skills declared by the template. The following excerpt contains the role definition and permission confirmation checklist:

```markdown
# Code Reviewer Agent

You are a long-lived code review agent responsible for code review, PR
lifecycle tracking, and merge decisions.

## Permission Confirmation Protocol

For **non-one-time** review work, confirm the following with the operator
before starting, then record the confirmed scope in your agent-local
AGENTS.md:

- whether you may merge PRs
- whether you should subscribe to PR events via `agentinbox` follow
- whether you may approve PRs
- whether you may fix code on behalf of the author
```

These rules require the agent to review code, follow PRs, and judge whether they can be merged. Before starting ongoing work, it must confirm permissions with you and record them in its own `AGENTS.md`. Installing a template does not automatically grant merge permission.

The rest of the file defines the workflow:

- **Ongoing follow-up:** Create a WorkItem for a PR and subscribe to new commits, CI, and review comments. When a new version arrives, recheck previous findings first. After merging or closure, complete the work item and clean up subscriptions.
- **Merge requirements:** All required CI checks on the final head must pass, with no unresolved blocking findings. Ordinary suggestions should not be treated as blockers, and GitHub's platform restrictions cannot be bypassed.
- **Escalation:** Leave large refactors, breaking API changes, and security-sensitive changes to the operator's judgment. Do not proactively fix code on the author's behalf without authorization.

Skills provide the specific review methods: `code-review` defines evidence, finding categories, and verification coverage; `github-review` handles GitHub context collection, deduplication, and review publication. `ghx` and `sview` support platform operations and source reading; `agentinbox` and `uxc` handle event integration.

Add your project requirements; you do not need to write a review prompt from scratch. To adjust general responsibilities or replace Skills, see the [Agent template guide](../guides/agent-templates.md).

## Confirm responsibilities and merge permissions before starting

Tell the reviewer which repository it is responsible for, the project requirements, and its operational permissions:

```text
Take responsibility for PRs in <owner/repo>. Subscribe to the repository, automatically review new PRs, and keep following them until merged or closed.
The local repository is at <absolute path>. Focus on compatibility and give feedback in Chinese; isolated testing, comments, and approvals are allowed.
You may merge directly when the final version's required checks pass, no blockers remain, and repository merge rules are satisfied.
Do not fix code on the author's behalf; ask me first about major or security-sensitive changes. Remember these long-term requirements.
```

Have the reviewer confirm and record these instructions in its own `AGENTS.md`. The same authorization applies to future PRs. Read the repository's own `AGENTS.md` first for build, test, and code conventions. If the team only allows squash merges, say so here too.

This authorization also requires the GitHub account to have the corresponding permissions and comply with repository protection rules. The template does not provide credentials. For a first trial, choose a familiar, low-risk PR and prepare an isolated testing environment.

## Subscribe to the repository to discover PRs automatically

After confirming responsibilities, connect the event source. The Holon repository's own `holon-reviewer` uses two subscription layers in its working rules:

- **Keep the repository subscription long-term** to discover newly opened PRs and let the reviewer start reviewing automatically.
- **Follow each PR separately** to receive subsequent commits, CI, and review comments; clean up that PR's subscription after merging or closure.

The template provides per-PR follow-up rules and integration Skills. The earlier authorization defines the repository and scope for automatically taking on work. External services still require separate authentication and subscriptions.

AgentInbox receives events and then notifies Holon to resume work; the GitHub integration uses UXC. Ask the reviewer to set up the integration using its included Skill:

```text
Use the agentinbox Skill to connect new-PR subscriptions for this repository and per-PR CI and comment follow-up. Tell me if tools or authentication are missing.
```

Following the Skill, the reviewer checks the services, repository access, and its own wake target, and identifies any installation or authentication you need to complete. Configure credentials through the tools' authentication flows; do not paste them into the conversation. For integration details, see the [AgentInbox onboarding guide](https://agentinbox.holon.run/guides/onboarding-with-agent-skill).

After setup, have it confirm that the repository discovery subscription is active. Do not assume a new event subscription fills in history. To take on existing open PRs, also say, “Take on the currently open PRs too.”

## Check whether it starts reviewing automatically

Wait for a new PR suitable for a trial run, and observe whether the reviewer discovers it from an event, automatically creates a WorkItem, and starts reviewing. Do not manually send the PR URL this time.

If reviewing does not start after a new PR appears, first check the repository discovery subscription, AgentInbox's event records, and the reviewer's wake target. Manually sending a PR can test a one-time review, but does not prove that automatic repository integration is active.

Open the WorkItem for this PR. You should find the reviewed head, findings and evidence, completed verification, and what it is waiting for next. When a fix arrives, the reviewer can compare the new version with the previous blockers.

Use this example to check the first review result:

```text
Reviewed version: <head SHA>
Blocking findings: <finding, location, and evidence>; or no blocking findings found in this review
Verification: <tests run and results>; <areas not covered>
Next step: wait for <author's fix / CI for the current head / maintainer's decision>
```

## Verify that follow-up reviews resume on their own

First let the existing CI finish and check whether the reviewer receives the event and updates the check results. Then wait for the author to submit an update normally, and observe whether it returns to the same PR's WorkItem to review the new version. Do not send “please continue” in between. Check both CI notifications and new commits; the former cannot replace the latter.

Check the re-review record:

- It states the new head, and the cited CI belongs to that version.
- It explains whether each previous finding is fixed or still present, with new findings listed separately.
- It clearly states what it is still waiting for or what decision it needs from you.

If AgentInbox has received the event but the reviewer has not resumed, first check the wake target, whether the runtime is online, and the work item's wait condition. If it resumes but still cites old CI, ask it to check the commit again. Sending “continue” can move work forward temporarily, but cannot replace this integration verification.

The second version of PR #2854 fixed the previous problem but introduced another error. The re-review record needs to distinguish the two so the author knows what to change next.

## Merge and clean up

Once the final head's required checks pass, no blockers remain, and repository rules are satisfied, the reviewer can merge directly under its authorization. For changes requiring escalation, or PRs lacking the required permissions or checks, it should explain why and leave the decision to you.

The final report should state the actual merge result, final commit, and unverified areas. After the PR is merged or closed, the reviewer confirms the terminal state, completes the WorkItem, and cleans up that task's subscriptions as required by the template; shared event sources remain for other work.

Keep the repository subscription so the reviewer can continue taking on new PRs and involve you only when a decision is needed. To retain manual merging, change the long-term authorization to “Notify me when merge conditions are met; I will merge it.”

## Case sources

The PR #2854 case uses the complete work plan and GitHub snapshots of commits, reviews, checks, and the merge. The case's tests were not rerun for this article, and subscription cleanup results were not independently verified.
