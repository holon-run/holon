---
title: "Holon's WorkItem architecture: separating work state from conversation"
summary: "Follow an interface migration from launch to retirement to see how WorkItem differs from Goal and Loop: it keeps objectives, progress, external dependencies, and delivery together across release cycles."
order: 20
---

# Holon's WorkItem architecture: separating work state from conversation

<img src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="Abstract illustration: flowing blue-green lines pass through stable geometric structures, expressing changing context with a structure that holds work state." decoding="async" fetchpriority="high">

> The interface migration and two-PR workflow in this article are design examples, not records of actual runs.

## As the river flows, what does the work leave behind?

Think of the tokens an LLM reads and generates as a river. Questions, reasoning, command output, and corrections flow past. Some matter only at that moment; others determine whether the next step will be correct.

This matters especially when work depends on someone else. Consider a code review agent, which we will call the reviewer. It reads thousands of lines of diff, finds a missing permission check, and waits for the author to fix it. Meanwhile, it reviews other PRs. By the time the original PR is updated, the initial analysis may have left the model's context window. The chat history may still exist, but rereading it from the beginning each time is impractical.

A WorkItem is like a boat floating on the river. What is worth keeping needs to be brought aboard: the review's scope, the issues found, the corresponding commits, the evidence still missing, and when to return. The river keeps flowing, while the record carrying this work remains.

When the agent returns, it needs to know where the work stands and find the materials needed to resume the review. How should the system preserve those records between executions, and when should it schedule the agent to continue?

## From continued execution to responsibility across cycles

Others have explored ways to keep agents working beyond a single response.

**Ralph Loop: keep iterating until the completion condition is met.** Claude Code's Ralph Wiggum plugin, for example, feeds the task prompt back in when the agent tries to stop. The agent takes another pass using its modified files and test results. Completion conditions and an iteration limit constrain the loop. This suits work that can move straight from implementation to testing and fixes.[1]

**Codex Goal: keep the objective active after a turn ends.** Codex's Goal extension stores the objective as persistent thread-level state and considers budget constraints when deciding whether to continue during idle periods. An objective recorded this way guides later execution instead of remaining only in an earlier user message. This describes the mechanism in the source code examined; it does not imply that every version exposes it as a default product feature.[2]

**Claude Code `/loop`: check again after an interval.** This approach fits following deployments, builds, or PRs: scheduling handles the checks instead of requiring a person to give each reminder. Unlike Ralph's continuous iteration, its focus is when to run again. The official documentation also distinguishes in-session loops, event integration, and scheduling independent of a session.[3]

These mechanisms help agents continue after a turn. But for some work, the next step does not depend on whether the agent can try again.

### The code is finished; the interface migration is not

Suppose you assign an interface migration to an agent: develop the new interface, follow the server release and client upgrades, and continue until the old interface can be retired under the agreed conditions. The acceptance objective is to complete the migration, not just submit code that passes tests.

- **Develop the new interface.** The agent changes the implementation, runs tests, and fixes problems. Continuous iteration can advance this stage.
- **Wait for the server release.** The code has merged, but the release window has not arrived. The agent records the version awaiting release, verification results, and post-release checks, then turns to other work. After receiving a release notification, it confirms that the new interface is available.
- **Follow client upgrades.** After the server goes live, client teams still need to adapt and release their clients, and users need time to upgrade. The agent retains each client's migration progress and unmet conditions, following up when a version is released or an agreed recheck time arrives.
- **Confirm retirement of the old version.** After the agreed observation period, it checks calls to the old version, migration acceptance criteria, and retirement approval. It then either acts within its authorization or delivers a retirement recommendation. A client releasing a new version does not mean every caller has migrated.

This work may span weeks, with several executions and long periods when there is nothing to do. More model turns cannot bring the release window forward or upgrade a client on another team's behalf. The objective "complete the interface migration" alone does not record what each stage has verified, who still needs to act, or what should trigger the next follow-up.

A scheduled loop can handle rechecks. To hand off the whole migration, you also need to save each stage's progress, connect external events to the work, distinguish waiting from completion, and find the right records on return. **WorkItem keeps this information in a persistent work item, independent of any single execution. The runtime manages its waits, resumption, and completion.**

This example requires connecting release notifications and upgrade-status queries, and agreeing on recheck times. During a wait, the migration work still exists, and the same agent can handle other assignments. When it resumes, it follows the original objective and records, without the user having to explain again which client is still pending.

Loop chiefly arranges repeated execution. Goal retains the objective to pursue. WorkItem tracks an assignment from the moment it is accepted through verification and delivery. Implementation and testing can use a loop; the migration has an objective throughout; and each stage's progress, dependencies, and delivery stay with the work item.

## Separate the responsible agent, the work, and each execution

Code review shows how these three fit together. Each Holon agent has its own AgentHome for responsibilities, memory, and materials. Its `AGENTS.md` records the long-term role: what it handles, how it works, and the limits of its existing authorization.

For a reviewer, these agreements might include checking code risks, drawing conclusions from tests and change records, and knowing when human confirmation is required. They do not expire when a particular PR ends.

Within this long-term role, you give it a specific assignment: review a PR, follow revisions and CI, and deliver a conclusion when agreed conditions are met. If merge permission was granted beforehand, merging can also belong to this assignment.

After this PR ends, the reviewer takes on the next review; the corresponding WorkItem ends after delivery. One WorkItem can span multiple execution rounds: first read the diff, wait for revisions, then recheck the code and test results.

A test process can finish while the review remains open. An agent can stop its current turn while still owning the assignment. The work ends only when the agreed objective has been verified as met.

A WorkItem must give later executions enough information to answer four questions:

| Question | Review example |
| --- | --- |
| What must be delivered? | Check the specified PR and give a conclusion or merge within the authorized scope |
| Where does the work stand? | The authorization path has been checked; the export interface lacks a permission check |
| Why can the next step not happen yet? | Waiting for the author to submit revisions, or for CI results on the current commit |
| Where should work resume? | Read the review record, check the new commit, and revisit the original issue |

These records need not live in one file or duplicate the whole conversation and every log. They need to belong to the same work item and be retrievable when it resumes.

This also determines granularity: another PR has its own review objective and suits a separate work item; "read one more file" is just an action within the current work. A question that can be answered or a small change that can be finished in this turn usually does not need an additional WorkItem.

## The model judges the next step; the runtime remembers how to continue

If an agent only says in its reply, "look again after the author fixes it," a person can understand, but the system may not know when to schedule another execution. The agent needs to register with the runtime which work is waiting for what, and under what conditions it can continue.

<img src="/assets/work-item-architecture-en.png" width="800" height="430" alt="WorkItem architecture: the agent decides the next step and the runtime registers work state. Objectives, progress evidence, and resumption conditions persist across turns. When a signal arrives, the runtime schedules execution and the agent reads the relevant records." loading="lazy" decoding="async">

*Figure 1: work records persist across turns. The lower part shows resumption after waiting; work that can continue does not need to wait for an external signal first.*

The agent understands the objective, analyzes evidence, and decides what still needs doing. The runtime saves work state and schedules later execution according to waiting conditions and arriving signals. When it runs again, the agent reads the records and rechecks the facts.

**Unfinished work is not necessarily ready to run.** An assignment waiting for CI remains open, but repeated model calls may accomplish nothing until results arrive. Opening a work item to inspect progress should not automatically clear its wait either.

**A signal is not proof of completion.** A new-commit notification gives the reviewer a reason to look again, not evidence that the permission issue is fixed. Waiting and scheduling mechanisms determine when to return. The agent then checks the evidence against the acceptance criteria.

## Restore context from the work record

Suppose the reviewer has recently been handling PR #102 when PR #101 is updated. Reading only the latest conversation turns can easily bring back details of #102 while missing #101's unresolved permission issue.

When resuming #101, the agent first reads that WorkItem's records to confirm the objective, authorization, and unresolved issues, then retrieves relevant evidence as needed. A review record can be short:

> **Objective**: follow PR #101 and complete the review and delivery under the agreed conditions.
>
> **Progress**: checked the interface entry points and authorization path; the export interface lacks a tenant permission check.
>
> **Evidence**: the reviewed commit, corresponding comments, and regression test results.
>
> **On return**: query the latest commit, review the incremental changes and original permission issue, then confirm check results.

The agent writes key findings and pointers to materials into the record, reading the full diff, logs, and test reports as needed. The record also needs to explain which judgments remain valid and which facts must be rechecked: passing tests on an old commit cannot replace verification of a new one.

## One reviewer, two interleaved PRs

Each PR corresponds to a WorkItem: A handles the review of PR #101, and B handles PR #102. Assuming PR update notifications and test results are already connected, the following diagram shows one possible processing order.

<img src="/assets/work-item-reviewer-sequence-en.png" width="800" height="410" alt="The same reviewer's current WorkItem switches from A to B, then back to A and B. Revisions to PR A make WorkItem A resumable, and completion of PR B's CI makes WorkItem B resumable. The runtime then schedules execution without immediate preemption. Below, separate records retain WorkItem A's wait for revisions and WorkItem B's wait for CI." loading="lazy" decoding="async">

*Figure 2: the same reviewer switches between A and B. Wakeup events appear above, and each work item's waiting records below.*

The reviewer selects or switches WorkItems with `PickWorkItem` to establish the current focus, or current WorkItem. The runtime can also select work that can continue when it wakes. **The current WorkItem indicates the focus of work, not that the work is running**: it may also be waiting. Switching focus does not clear another work item's progress or waiting conditions.

### A waits for revisions while the agent handles B

The reviewer finds a permission issue in A, records its findings and where to resume the review, and registers a wait for the author's revisions. A is unfinished, but there is temporarily no next step to take. The reviewer can now take on B instead of staying in A's conversation and repeatedly checking for new commits.

### An update to A does not mean dropping B

While handling B, an update notification for A arrives. In this example, the reviewer first finishes its current stretch of analysis for B, starts tests, and saves progress before waiting for B's test results. When scheduling conditions allow, it returns to A.

A's and B's waits each belong to their own work: A waits for revisions, B waits for tests. A's notification does not end B's test wait, and switching work does not automatically mark B complete.

Here, concurrency means first that one agent owns several unfinished work items. External CI and background tests can run in parallel without the agent running two model turns at once.

### Return to A, recheck, and deliver

The reviewer reads A's records, queries the current commit, revisits the original issue, and checks CI. If conditions remain unmet, it updates the record and keeps waiting. If they are met, it delivers within the assignment's scope. With prior merge authorization, it can merge after the recheck passes. Without that authorization, it delivers the review conclusion.

After A ends, B's records and wait still exist. When B's test results arrive, B resumes.

## Completion needs delivery; continuation needs evidence

A completed assignment should show whether the objective was met and what was delivered. For a review, that means identifying the commit checked, whether issues were addressed, the verification results, and the actions taken within the granted authority. A "completed" status alone is not enough for someone to take over.

If the report still says "continue following up later," check whether the original objective has really ended. Unfulfilled responsibilities should remain in unfinished work or have an explicit follow-up arrangement.

Every time work resumes, the agent must check the current version and external progress, and update plans that have changed. It should arrange rechecks for notifications that might have been missed. At delivery, state the acceptance evidence, results, and outstanding matters clearly, so the person taking over can judge whether the assignment is complete.

---

### Sources and further reading

The industry approaches above are summarized from materials consulted on 2026-09-12. The discussion focuses on mechanisms, not a full comparison of product capabilities:

1. [Ralph Wiggum plugin documentation](https://github.com/anthropics/claude-code/blob/main/plugins/ralph-wiggum/README.md): iteration when the agent tries to stop, completion conditions, and iteration limits.
2. [Codex Goal extension source](https://github.com/openai/codex/tree/95637f7056835fea66bdd0044414af480fc0fd74/codex-rs/ext/goal): a fixed commit as the basis for thread-level objectives, persistent records, and continuation during idle periods.
3. [Claude Code scheduled tasks documentation](https://code.claude.com/docs/en/scheduled-tasks): `/loop`, session scope, and other scheduling approaches.

For Holon's specific interfaces and behavior, see the [work item contract](/spec/work-items) and [waiting and wakeup](/spec/wake-and-continuation). The next article, [One PR, one work item](./one-pr-one-work-item), turns to the reviewer's actual configuration and workflow.
