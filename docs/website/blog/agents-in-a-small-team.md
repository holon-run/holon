---
title: "From personal AI tools to team collaboration: an agent-native case study"
summary: "How a small team moved from individual AI tools to shared agents, with 77 days of retained usage records, device investigations, and a hands-on test of silent piano keys."
order: 40
---

# From personal AI tools to team collaboration: an agent-native case study

![Team members, their personal AI tools, and shared team agents assemble work cards together, with the Holon logo at the bottom right.](/assets/team-shared-agents-cover.webp)

> Internal identifiers and environment labels have been omitted from the cases.

Since May 2026, I have worked with an AI hardware product team as a technical advisor, supporting its AI agents and server-side technology. The team has four members: one Android developer, one iOS developer, a product manager who also handles marketing, and a tester.

Initially, developers used coding tools such as Codex on their own computers. Device investigations, post-fix version checks, and test feedback still required someone to read logs, check GitHub, and follow the group chat.

We deployed Holon on a server so shared agents could take ongoing responsibility for investigation, review, operations, and acceptance follow-up. Developers continue using their own coding tools, while shared agents follow the team's investigation results, fix progress, and acceptance feedback. Here are two cases from that work.

## What is an Agent Native organization?

Industry discussion has begun moving from “giving employees AI assistants” to “how people and agents form teams.” In its 2025 Work Trend Index report, Microsoft used Frontier Firm to describe a new organizational form: people and agents work together, and teams organize more around goals rather than only by function.[^frontier]

In this small team, I use a more specific definition: **An Agent Native organization includes agents in the team when assigning responsibilities, work entry points, and handoffs, giving them explicit work roles.**

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

Model usage records for these six roles go back to **June 25, 2026**. Through **September 9**, a total of 77 days, the retained records show cumulative usage of about **7.764 billion tokens**: 7.694 billion input and 0.070 billion output. Input includes cache reads and cache writes; approximately 6.259 billion cache-read tokens are already included in the total.[^activity]

<picture class="team-diagram">
  <source media="(max-width: 1000px)" srcset="/assets/team-usage-en-narrow.svg">
  <img src="/assets/team-usage-en.svg" alt="About 7.764 billion tokens over 77 days, including cached input. The pie chart shows usage by role: code review 52.16%, incident investigation 24.76%, testing and acceptance 10.64%, collaboration assistant 4.31%, data analysis 4.31%, and product operations 3.82%.">
</picture>

*The pie chart shows each of the six roles' share of cumulative usage over the full reporting window. Values are rounded individually, so displayed values may not add up exactly to the total.*

Shared-agent activity appears in the records on all 77 days. Model usage is concentrated in code review, incident investigation, and testing and acceptance; the other roles participate as needed.

Within the same reporting window, we also compiled records left by the shared bot in the product's main repository. Filtering by the actual publication time of reports or reviews, then deduplicating by Issue or PR, gives the following coverage counts:[^coverage]

<div class="article-table" role="region" aria-label="Work records and counting methods" tabindex="0">

| Work record | Items covered | Counting method |
| --- | ---: | --- |
| Investigation and analysis reports | **858 Issues** | Identify explicit investigation report titles and evidence markers in the content; exclude comments that only announce the start of an investigation |
| Submitted reviews | **1,217 PRs** | Formal reviews with nonempty bodies; count each PR once even if reviewed multiple times |
| Acceptance and verification reports | **652 Issues** | Identify explicit acceptance or verification reports; a passing conclusion is not required |

</div>

Acceptance records include code checks, summaries of human results, and conclusions requiring retesting. The same item may appear in multiple categories, so the counts in the table are not added together.

## Case one: From device logs to an Issue a developer can act on

### First establish what actually happened

With bugs on client devices, the difficulty is often reconstructing the incident rather than describing the symptom. The user sees “nothing happens when I tap,” but the developer needs to know what actions preceded it, how far the client and server each got, and which step stopped producing results.

We built trace-log reporting to help reconstruct that sequence. Events recorded during a run connect user actions with system responses so developers can review the incident.

In our current workflow, a new log report triggers a webhook that notifies the investigation agent in Holon. In the server environment, the agent can directly read traces it is authorized to access. After analysis, it creates or updates a GitHub Issue, adds labels, and assigns developers. Investigation results for existing Issues go back into the original record, so developers do not have to look elsewhere for the analysis.

For the team, this removes one handoff that previously required a person to initiate it: waiting for a developer to have time to download logs and feed them to a personal AI tool. Investigation can begin when incident materials arrive, and the results stay in the GitHub Issues everyone already uses.

<picture class="team-diagram">
  <source media="(max-width: 1000px)" srcset="/assets/team-investigation-en-narrow.svg">
  <img src="/assets/team-investigation-en.svg" alt="The client generates a diagnostic report and uploads it to the server. A webhook notifies the trace report agent in Holon, which reads authorized traces, reconstructs the sequence, and organizes leads and logging gaps. It creates or updates a GitHub Issue, adds labels, and assigns developers for further diagnosis and fixes.">
</picture>

### Investigating an unresponsive “View full score” action

In one case, a user finished practicing a song in an Android lesson and tapped “View full score,” but the interface did not proceed. The user could not tell whether it was loading or stuck.

The Issue included clues from a diagnostic report. After analyzing the runtime records, the investigation agent noted that the client operation made no progress for a long time and also lacked visible feedback after cancellation.

When the developer took over, the Issue contained more than “the button does nothing.” It already had the original scenario, runtime clues, and cancellation and recovery paths worth examining. The developer then adjusted Android's logic for canceling previous operations and recovering through retries.

## Case two: The Issue is closed. Who follows up after release?

In our development workflow, the linked GitHub Issue closes when its PR merges. But the server has not yet been deployed, and the client build has not yet been released. The Issue list shows the work as finished, yet the tester's available version still lacks the fix. **A closed Issue does not mean deployed, released, or accepted.**

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

Code tests can detect unwanted commands. Confirming that a pressed key actually makes sound takes a person at the device. Using the fixed Android build on a device with the relevant new firmware, team members repeated the sequence of connecting the device, entering a lesson, lighting the keys, pressing them to produce sound, and interacting with the lesson. The developer recorded this physical-device confirmation and the retested version in the GitHub Issue.

By the time the testing agent took over, the hands-on test was already complete. It checked the fix and CI results, then combined code checks, CI results, and existing device feedback into the acceptance record. Developers do not need to wait for an agent assignment before debugging, and the agent does not need to make people repeat verification they have already done.

If device feedback is not yet available, the testing agent gives the tester the device requirements, testable version, and retest steps, then updates the acceptance record after the hands-on test is complete.

## From using AI individually to moving work forward together

Looking back at these two cases, shared agents handle work that needs continued attention through team handoffs: turning arriving incident logs into problems developers can take over; after code merges, continuing to wait for deployment and client releases and combining automated checks with hands-on tests in acceptance records. This work used to require someone to take the initiative to keep it moving; it now has an agent explicitly responsible for it.

Personal AI tools help developers expand the range of work they can handle. I also saw developers who had worked separately on iOS and Android begin delivering complete features across platforms. Shared agents keep investigation leads, fix progress, and test feedback in the team's shared records for the next person to continue the work.

This is what Agent Native practice means to me: include agents in everyday responsibilities, clarify what they remain responsible for, when they receive work, and to whom they hand results. Everyone still uses AI for their own tasks, and also works with shared agents to follow the team's problems from discovery through verification.

Holon provides long-lived agents and [WorkItems](./why-work-items) for this division of work: roles can persist, while the goals, plans, and follow-up items of specific work can be saved. After a model execution ends, work waiting for deployment or human feedback remains and can resume when its conditions are met.

Investigation and acceptance are the first two cases we have explored. Later, I would also like to write about code review, product analysis, and how developers deliver a complete feature across platforms.

If you also want to begin with a shared team runtime, see the [Remote access and service deployment guide](../guides/connect-remote-runtime.md) (in English) to learn how members can connect to the same Holon service; then use the [WorkItem guide](../reference/work-items.md) to organize work requiring ongoing follow-up. Choosing one recurring team task with clear handoff boundaries makes it easier to test whether this collaboration is useful than assigning a full set of agent roles at the start.

---

[^frontier]: Microsoft WorkLab, *2025: The year the Frontier Firm is born*, April 23, 2025.

[^activity]: Usage is deduplicated and aggregated from retained records for six shared agents, from June 25, 2026 at 00:00 to September 10 at 00:00 Beijing time. It excludes personal coding tools and records that were not retained. Input includes cache reads and writes. The original figures in hundreds of millions and percentages are rounded to two decimal places; billion-token equivalents here retain that precision.

[^coverage]: Coverage counts use the shared bot's investigation and acceptance reports and formal PR reviews in the product's main repository, deduplicated by Issue or PR over the full window. Reports are filtered by titles and content markers; the PR search covers records created from May through September 2026.

[^api-check]: Request and response check results come from work records saved by the testing agent and GitHub Issue comments.

**Sources:** The team practice comes from my own participation. Data and cases were compiled from retained node records, GitHub Issues, comments, and fix records.
