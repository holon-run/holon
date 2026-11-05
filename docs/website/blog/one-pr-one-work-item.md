---
title: "Beyond a one-time review: build a reviewer that follows PRs with Holon"
summary: "Create a reviewer with built-in working rules and Skills, set its responsibilities and merge permissions, and have it discover PRs and follow fixes and CI updates."
order: 30
date: 2026-09-13
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

Following a PR across new commits and CI runs—without re-pasting links or repeating resolved comments on every push—relies on three pieces working together: **a role contract from the template, two layers of event subscriptions, and one durable WorkItem per PR**.

> To set this up in your own repository step by step, follow the guide: [Build a local Code Reviewer that follows PRs](../guides/build-code-reviewer.md).

## 1. Role contract and permission confirmation

The `code-reviewer` template initializes the agent's working rules (`AGENTS.md`) and installs the required Skills (`code-review`, `github-review`, `ghx`, `sview`, `agentinbox`, and `uxc`). Its core safeguard is the **Permission Confirmation Protocol**:

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

Installing the template never grants merge or push permissions by default. In your first conversation, you simply tell the agent in plain language which repository to watch, when it is allowed to approve or merge, and what changes require human review. The agent attaches the repository and records those standing rules in its own `AGENTS.md`:

- **Merge requirements:** All required CI checks on the final head must pass, with no unresolved blocking findings. Ordinary suggestions should not be treated as blockers, and GitHub's platform restrictions cannot be bypassed.
- **Escalation:** Leave large refactors, breaking API changes, and security-sensitive changes to the operator's judgment. Do not proactively fix code on the author's behalf without authorization.

## 2. Two subscription layers and one WorkItem per PR

To follow PRs autonomously, `holon-reviewer` separates repository discovery from per-PR tracking through AgentInbox:

- **Keep the repository subscription long-term** to discover newly opened PRs and let the reviewer start reviewing automatically.
- **Follow each PR separately** to receive subsequent commits, CI, and review comments; clean up that PR's subscription after merging or closure.

When a new PR opens, the reviewer creates a dedicated [WorkItem](why-work-items.md) that records the reviewed `head SHA`, blocking findings with evidence, local verification results, and the exact wait condition:

```text
Reviewed version: <head SHA>
Blocking findings: <finding, location, and evidence>; or no blocking findings found in this review
Verification: <tests run and results>; <areas not covered>
Next step: wait for <author's fix / CI for the current head / maintainer's decision>
```

## 3. Revision-aware re-review and merge cleanup

Because review state lives in the PR's WorkItem rather than a transient chat scrollback, every CI update or new push wakes that same WorkItem without anyone typing `"please continue"`:

- **Exact revision matching:** Each re-review records the new `head SHA` and checks CI for that commit rather than stale results from an earlier push.
- **Deduplicated delta feedback:** As in the second revision of PR #2854 (`19f390ca`), the reviewer first checks whether earlier blockers are resolved—without re-posting resolved threads—and lists any newly introduced issues separately so the author knows exactly what remains.
- **Clean closure:** Once required checks pass on the final head and no blockers remain, the reviewer merges (or notifies a maintainer), completes the WorkItem, and removes that PR's subscription while keeping the repository-level watch active for future PRs.

## Next step: Set up your own Code Reviewer

To set up this workflow in your own repository—creating the agent in the Web GUI and asking it in conversation to attach your codebase, record permissions, and connect event subscriptions—follow the step-by-step guide:

- **[Build a local Code Reviewer that follows PRs](../guides/build-code-reviewer.md)**

## Case sources

The PR #2854 case uses the complete work plan and GitHub snapshots of commits, reviews, checks, and the merge. The case's tests were not rerun for this article, and subscription cleanup results were not independently verified.
