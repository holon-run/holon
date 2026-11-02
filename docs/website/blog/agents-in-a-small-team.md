---
title: "From personal AI tools to team collaboration: an agent-native case study"
summary: "How a small team moved from individual AI tools to shared agents, with 131 days of retained usage records, wake-source breakdown, device investigations, and a hands-on test of silent piano keys."
order: 40
date: 2026-09-13
---

# From personal AI tools to team collaboration: an agent-native case study

![Team members, their personal AI tools, and shared team agents assemble work cards together, with the Holon logo at the bottom right.](/assets/team-shared-agents-cover.webp)

Since May 2026, we have carried out this practice in an AI hardware product team of five: one Android developer, one iOS developer, a product manager who also handles marketing, a tester, and a technical advisor supporting AI agents and server-side technology.

Initially, developers used coding tools such as Codex on their own computers. Device investigations, post-fix version checks, and test feedback still required someone to read logs, check GitHub, and follow the group chat.

We deployed Holon on a server so shared agents could take ongoing responsibility for investigation, review, operations, and acceptance follow-up. Developers continue using their own coding tools, while shared agents follow the team's investigation results, fix progress, and acceptance feedback. Here are two cases from that work.

## What is an Agent Native organization?

Industry discussion has begun moving from “giving employees AI assistants” to “how people and agents form teams.” In its 2025 Work Trend Index report, Microsoft used Frontier Firm to describe a new organizational form: people and agents work together, and teams organize more around goals rather than only by function.[^frontier]

In this practice, we use a more specific definition: **An Agent Native organization includes agents in the team when assigning responsibilities, work entry points, and handoffs, giving them explicit work roles.**

Take fault investigation as an example. If a developer must find the logs each time, paste them into a chat window, and ask “help me analyze this,” AI is still mainly a personal tool. Giving this work to an investigation agent on the team requires answering more questions: Where do new logs come from? What event notifies it? What materials can it access? Where does it write its analysis, and who takes over next?

In daily work, this means arranging the following:

- **Ongoing responsibilities.** The investigation agent organizes incident evidence into an investigable problem; the testing agent follows verification after a fix. The end of a conversation does not end that responsibility.
- **Shared team work records.** Agents receive work from log events, GitHub Issues, code changes, and deployment results, and write their output back to existing team records rather than leaving it only in someone's chat window.
- **Human handoffs and returning feedback.** Work requiring physical device operation goes to the tester; product decisions go to the product manager. After a person provides results, the agent continues updating the work.

For example, the testing agent handles version checks and API checks, while the tester judges interaction and experience on real devices. Together, they complete acceptance testing.

## What work does Holon do in the team?

Server-side Holon is the runtime for these shared agents and does not replace developers' local coding tools. The agents participate in product development, separate from the product's user-facing AI features.

The shared roles currently used by the team include:

<div class="article-table" role="region" aria-label="Shared agent responsibilities" tabindex="0">

| Shared agent responsibility | Incoming work | What it leaves for the team |
| --- | --- | --- |
| Trace investigation | Log reports, GitHub Issues | Incident analysis, diagnostic leads, and questions for developers to confirm |
| Code review | PRs, new commits, and CI changes | Review comments and items still needing follow-up after changes |
| Testing and acceptance | Issue closure, completed test-environment deployments | Automated check results, manual retest requirements, and acceptance records |
| Product operations | Deployment requests and inspection schedules | Testable environments, deployment records, and anomaly reports |
| Data analysis | Analysis requests | Product metrics and conversation statistics for team discussion |
| Team collaboration assistant | Group messages and coordination requests | Organized requirements, project information, and delivery materials |

</div>

Model usage records for these six roles go back to **June 25, 2026**. Through **November 2**, a total of 131 days, the retained records show cumulative usage of about **20.431 billion tokens**: 20.195 billion input and 0.236 billion output. Input includes cache reads and cache writes; approximately 17.282 billion cache-read tokens (85.6% of input) are already included in the total.[^activity]

<picture class="team-diagram">
  <source media="(max-width: 1000px)" srcset="/assets/team-usage-en-narrow.svg">
  <img src="/assets/team-usage-en.svg" alt="About 20.431 billion tokens and 39,809 runtime turns over 131 days, including cached input. The left pie chart shows usage by role: code review 41.74%, incident investigation 25.46%, testing and acceptance 23.77%, product operations 3.40%, team assistance 3.37%, and data analysis 2.26%. The right bar chart shows turns by wake source: external webhooks 59.16%, runtime continuation and queue scheduling 36.98%, human operator prompts 2.88%, and scheduled timers 0.99%.">
</picture>

*The left panel shows each role's share of cumulative token usage over the full reporting window; the right panel shows what woke the 39,809 runtime turns. Values are rounded individually, so displayed values may not add up exactly to the total.*

Shared-agent activity appears in the records on all 131 days. Model usage is concentrated in code review, incident investigation, and testing and acceptance—with acceptance testing rising from 10.64% in the first 77 days to 23.77% as release verification intensified. Across the **39,809 runtime turns**, **59.16% (23,551 turns)** were triggered directly by external webhooks such as device log uploads, GitHub events, and CI callbacks (batched automatically by the runtime from 34,876 deliveries during busy windows), **36.98% (14,720 turns)** came from task results, internal follow-ups, and WorkItem rechecks, **0.99% (393 turns)** came from scheduled timers, and only **2.88% (1,145 turns)** came from direct human prompts. Over 97% of turns ran without someone typing into a chat box, and the six agents advanced and completed **3,986 WorkItems** across the window.

Within the same reporting window, we also compiled records left by the shared bot in the product's main repository. Filtering by the actual creation or publication time of Issues, reports, or reviews, then deduplicating by Issue or PR, gives the following coverage counts:[^coverage]

<div class="article-table" role="region" aria-label="Work records and counting methods" tabindex="0">

| Work record | Items covered | Counting method |
| --- | ---: | --- |
| Automatically created Issues | **1,044 Issues** | Issues created directly in the main repository by investigation and other shared agents from incoming reports or inspection leads |
| Investigation and analysis reports | **897 Issues** | Identify explicit investigation report titles and evidence markers in the content; exclude comments that only announce the start of an investigation |
| Submitted reviews | **1,666 PRs** | Formal reviews with nonempty bodies; count each PR once even if reviewed multiple times |
| Acceptance and verification reports | **931 Issues** | Identify explicit acceptance or verification reports; a passing conclusion is not required |

</div>

Acceptance records include code checks, summaries of human results, and conclusions requiring retesting. The same item may appear in multiple categories, so the counts in the table are not added together.

## Case one: From device logs to an Issue a developer can act on

### First establish what actually happened

With bugs on client devices, the difficulty is often reconstructing the incident rather than describing the symptom. When a user reports that “the app stopped responding halfway through piano practice,” a text description alone cannot tell whether it is a hardware fault or a software bug; the developer needs to know what actions preceded it, how far the device and client each got, and which step stopped progressing.

We built trace-log reporting to help reconstruct that sequence. Events recorded during a run connect user actions with system responses so developers can review the incident.

In our current workflow, a new log report triggers a webhook that notifies the investigation agent in Holon. In the server environment, the agent can directly read traces it is authorized to access. After analysis, it creates or updates a GitHub Issue, adds labels, and assigns developers. Investigation results for existing Issues go back into the original record, so developers do not have to look elsewhere for the analysis.

For the team, this removes one handoff that previously required a person to initiate it: waiting for a developer to have time to download logs and feed them to a personal AI tool. Investigation can begin when incident materials arrive, and the results stay in the GitHub Issues everyone already uses.

<picture class="team-diagram">
  <source media="(max-width: 1000px)" srcset="/assets/team-investigation-en-narrow.svg">
  <img src="/assets/team-investigation-en.svg" alt="The client generates a diagnostic report and uploads it to the server. A webhook notifies the trace report agent in Holon, which reads authorized traces, reconstructs the sequence, and organizes leads and logging gaps. It creates or updates a GitHub Issue, adds labels, and assigns developers for further diagnosis and fixes.">
</picture>

### Investigating a practice session that stopped responding mid-song

In one case, a user was practicing in a lesson when the app suddenly stopped responding halfway through. From that symptom alone, it was hard to tell whether the piano key sensors had failed, the Bluetooth connection had dropped, or the app's lesson logic was stuck.

The report arrived with a runtime trace. After reading the trace and reconstructing the timeline, the investigation agent found that the Bluetooth connection and raw key signals were still arriving normally, but after an interaction state transition on the client, the previous state had not been cleared—blocking subsequent follow-along evaluation without showing a timeout prompt.

When the developer took over, the Issue contained much more than “it stopped responding halfway through.” Hardware failure had already been ruled out, and the Issue included the action sequence leading up to the stall, the client state transitions, and the cancellation and recovery paths worth checking. The developer then fixed how old operations are cleared and recovered during lesson state transitions.

## Case two: The Issue is closed. Who follows up after release?

In our development workflow, the linked GitHub Issue closes when its PR merges. But the server has not yet been deployed, and the client build has not yet been released. The Issue list shows the work as finished, yet the tester's available version still lacks the fix. A closed Issue does not mean deployed, released, or accepted.

This gap can span several merges, accumulating closed but unverified issues. The testing agent needs to keep following this work, listen for server deployment completion and client version release events, and resume acceptance testing when the relevant version is testable. The operations agent proceeds with deployment according to schedules or human requests; the testing agent continues checking versions and fixes.

Acceptance covers the changes actually delivered in a version cycle. When a deployment or release event arrives, the testing agent checks which changes the version contains, links the closed Issues among them, and prepares a version acceptance checklist. Grouping only by Issue closure date is not enough: fixes that have merged but are not in the current testable version must remain pending acceptance.

<picture class="team-diagram">
  <source media="(max-width: 1000px)" srcset="/assets/team-collaboration-en-narrow.svg">
  <img src="/assets/team-collaboration-en.svg" alt="Merging a PR closes linked Issues, but fixes have not yet been deployed or released. The testing agent keeps following them, listens for server deployment completion and client version release events, and waits according to each change's dependencies. Once the relevant version is testable, it checks the changes and closed Issues actually included and creates a version acceptance checklist; fixes not in the version keep waiting. Items receive automated agent checks or hands-on human tests. Results go back to the original Issues and version acceptance records, with the version, scope, and evidence sources stated.">
</picture>

*Server changes wait for the relevant deployment; client changes wait for the relevant version release. Cross-client/server issues require checking conditions on both sides. Not every acceptance check must wait for both events.*

Once the checklist is ready, separate what the agent can check from what needs a person at a device. The following two independent cases illustrate these verification methods.

### Admin redirects: The agent checks requests and responses

One server-side fix involved redirects in the admin interface: when an old URL was visited, the redirected URL lost query parameters. For example, information specifying a return location did not survive the redirect.

This kind of problem can be checked directly through requests and responses, without a phone screen or hardware operation. The testing agent's records show that it checked four access scenarios in the test environment: a login path with parameters, a root path with encoded parameters, a subpath with multiple parameters, and a path without parameters. The recorded results showed that the redirect URL correctly preserved the expected parameters, and it updated the acceptance status accordingly.

The agent wrote the conditions and results of these four requests back to the GitHub Issue for developers and testers to read directly.[^api-check]

### Do the piano keys make sound? Check on a real device

Another problem occurred on a piano used with the Android app. After a firmware upgrade, the guide lights on the keys lit up normally when entering a lesson, but pressing a lit key produced no piano sound. This did not happen with the old firmware.

The developer found that Android sent conflicting mode commands when connecting to the device. The new firmware handled key presses under one of those modes, so pressing a key no longer directly produced a single note. The fix removed the redundant commands and added regression tests to prevent them from returning to the connection flow.

Code tests can verify that unwanted commands are no longer sent, but they cannot confirm on behalf of a person that pressing a key actually produces sound. Once the client build containing the fix was packaged, the testing agent checked the code changes and CI results, determined that verification required physical hardware and audible confirmation, and updated the Issue with the firmware requirements, testable build, and retest steps—adding labels for human review and on-device retesting.

Picked up via those labels, the tester installed the fixed Android build on a piano running the new firmware, walked through connecting the device, entering a lesson, lighting the keys, pressing keys for sound, and completing lesson interactions, and wrote the hands-on confirmation back to the Issue. The testing agent then combined the code checks, CI results, and on-device feedback to close out the acceptance record.

## From using AI individually to moving work forward together

Looking back at these two cases, shared agents handle work that needs continued attention through team handoffs: turning arriving incident logs into problems developers can take over; after code merges, continuing to wait for deployment and client releases and combining automated checks with hands-on tests in acceptance records. This work used to require someone to take the initiative to keep it moving; it now has an agent explicitly responsible for it.

Personal AI tools help developers expand the range of work they can handle. During this practice, developers who had previously worked separately on iOS and Android also began delivering complete features across platforms. Shared agents keep investigation leads, fix progress, and test feedback in the team's shared records for the next person to continue the work.

This is what Agent Native practice means to us: include agents in everyday responsibilities, clarify what they remain responsible for, when they receive work, and to whom they hand results. Everyone still uses AI for their own tasks, and also works with shared agents to follow the team's problems from discovery through verification.

Holon provides long-lived agents and [WorkItems](./why-work-items) for this division of work: roles can persist, while the goals, plans, and follow-up items of specific work can be saved. After a model execution ends, work waiting for deployment or human feedback remains and can resume when its conditions are met.

Investigation and acceptance are the first two cases we have explored. Next, we will also share our practices on code review, product analysis, and how developers deliver a complete feature across platforms.

If you also want to begin with a shared team runtime, start with [Share AI assistants with your team](../guides/share-with-your-team.md) for the end-to-end setup—covering [remote runtime deployment](../guides/connect-remote-runtime.md), [team OIDC single sign-on](../guides/configure-oidc-authentication.md), and shared Web GUI workflows—then use the [WorkItem guide](../reference/work-items.md) to organize work requiring ongoing follow-up. Choosing one recurring team task with clear handoff boundaries makes it easier to test whether this collaboration is useful than assigning a full set of agent roles at the start.

---

[^frontier]: Microsoft WorkLab, *2025: The year the Frontier Firm is born*, April 23, 2025.

[^activity]: Usage and turn counts are deduplicated and aggregated from retained records for six shared agents, from June 25, 2026 at 00:00 to November 2, 2026 Beijing time. They exclude personal coding tools and records that were not retained. Input includes cache reads and writes, and every external-webhook turn matches a recorded external trigger delivery. The original figures in hundreds of millions and percentages are rounded to two decimal places; billion-token equivalents here retain that precision.

[^coverage]: Coverage counts use Issues created by the shared bot, investigation and acceptance reports, and formal PR reviews in the product's main repository, deduplicated by Issue or PR over the full window. Reports are filtered by titles and content markers across records created or updated during the reporting window.

[^api-check]: Request and response check results come from work records saved by the testing agent and GitHub Issue comments.

**Sources:** Data and cases were compiled from the team's retained node records, GitHub Issues, comments, and fix records.
