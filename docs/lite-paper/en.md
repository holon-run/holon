---
lang: en
version: 0.13
updated: 2026-10-21
status: draft
title: Holon: Create and run AI assistants
---

# Holon: Create and run AI assistants

**Assign real work, track progress, and let your assistants follow up.**

Holon is open-source software for individuals and teams to create and run AI assistants on their own computers or servers. Give an assistant a goal, materials, and completion criteria. It uses configured tools to do the work and leaves results with supporting evidence.

Start with investigating a problem or reviewing a code change. With tools and events connected, an assistant can also take on ongoing mail triage or issue follow-up. Progress stays with the work while it waits for a reply or check result, ready to continue when the condition is met.

**Hand over a task**\
State what to solve, what to deliver, and which decisions need your approval.

**Pick up from existing progress**\
Goals, findings, and next steps are saved with the work, rather than explained from scratch each time.

**Step in when needed**\
Inspect results and evidence, add context, change direction, and make decisions that need your judgment.

![External events, schedules, and task assignments enter Holon. The agent checks progress, decides, acts, and records results. It waits when needed, resumes when conditions are met, and delivers when the work meets its requirements.](assets/work-loop-en.png)

## Give an assistant one concrete task

You do not need to connect every business system first. Choose work with clear inputs and completion criteria, then check whether the assistant delivers something useful.

**1. Set the requirements.** Provide the goal, materials, and acceptance criteria. For example: "Investigate the anomaly in this device log. Deliver reproduction clues, possible causes, and open questions. Do not change code."

**2. Work with tools.** The assistant reads materials, runs checks, and organizes findings in its configured environment. File changes and system access remain subject to configured permissions and your authorization.

**3. Inspect progress.** Read conclusions, next steps, and waiting reasons first. Inspect tool activity and source evidence when needed, rather than relying on a conversational answer alone.

**4. Add context or make a decision.** Supply missing material, adjust scope, or decide a disputed trade-off. Findings and next steps remain available while the assistant waits for your reply.

**5. Review the delivery.** Check the report, files, or changes against your requirements. Failed checks and unverified points should be explicit, not hidden behind a claim of completion.

### Separate the role, method, and work

An assistant in Holon is called an **agent**. Each keeps its own role, memory, and work records. Create one from a template or configure it for your needs.

| Component | What it defines | Example |
| --- | --- | --- |
| **Agent Template** | The assistant's role | Problem investigation or code review |
| **Skills** | Methods for doing that work | Log analysis steps or a review checklist |
| **WorkItem** | The goal, plan, and progress of work that needs follow-up | Following this device anomaly through investigation |

Workspaces connect the assistant to real project directories and materials. One assistant can switch between workspaces as a task requires; coding tasks can also use Git worktree for separate code directories.

## How work keeps moving

Following up does not mean generating text nonstop. Holon records what work is waiting for and resumes it when the corresponding result or event arrives.

**Wait for execution results.** While a command or delegated task is running, wait for its result. Read that result before deciding the next step.

**Wait for your judgment.** When materials are missing or approval is needed, bring the question back to you. A reply lets the assistant continue with the existing goal and findings.

**Respond to external changes.** Webhooks can deliver system messages or prompt an assistant to check for updates. Code changes, check results, or order updates can become follow-up signals.

**Check on a schedule.** Timers support one-off reminders and recurring checks for status reviews, summaries, or pending work.

Each ongoing piece of work can have its own WorkItem, recording the goal, plan, checklist, waiting reasons, and completion report. The assistant uses those records when new messages and results arrive. Readers can inspect the report first, then check sources and tool activity.

### What ongoing work requires

- **Keep the host running.** Closing a browser or disconnecting a terminal does not stop the background service. Turning off the host or stopping Holon's service prevents further execution.
- **Configure the connections you need.** Systems with Webhook support can send notifications. Reading mail, filtering content, or converting formats requires suitable tools or adapters. You can supply new results manually first.
- **A notification is not authorization.** System messages and human instructions retain their sources. Connecting events does not grant permission to publish, merge code, or access other systems.

Self-hosting means Holon and its working environment run on your chosen machine. **It does not mean all model inference is local.** Data sent in model requests depends on the provider and configuration you choose.

## An assistant with an ongoing review role

**For individual developers and teams that need ongoing code review.**

Create an assistant from a review template, configure skills and a project workspace, and give it an ongoing responsibility. Start by supplying one PR manually; connect GitHub events later:

> Review this repository's pull requests (PRs), focusing on compatibility and regressions. Follow up when code changes or automated checks (CI) finish. Ask me about disputed trade-offs. Keep a conclusion and supporting evidence for each PR.

Each delivery should state the findings, evidence, checks performed, and decisions still needed. Once events are connected, CI can send its result through a Webhook. On failure, the assistant investigates; on success, it checks for remaining issues.

![A new PR, code changes, and CI results prompt successive reviews. Findings and progress stay in the same work item through waiting, re-review, and delivery.](assets/pr-review-en.png)

**Each PR has its own work record.** The assistant checks revised code against earlier findings, keeps next steps while waiting for changes or CI, and delivers a report once the requirements are met. A review conclusion does not replace a person's merge decision.

Close the terminal and return later through a browser. While the host and Holon's background service remain running, the agent can keep receiving updates and following up.

*Illustrative workflow. Automatic GitHub updates require tools, account permissions, and event integration. You can also supply new results manually. Publishing or merging code requires authorization.*

## Use it on your own or with a team

On your own, keep an assistant responsible for a project and return to its progress later. With a team, run Holon on a server and give shared assistants investigation, review, or acceptance-testing roles. Members use a browser or terminal to see the same progress, add context, or take over.

[A four-person AI hardware team's experience](../website/blog/agents-in-a-small-team.md) illustrates two handoffs:

**Investigation agent: turn field evidence into an actionable issue.** It organizes device logs and findings into an issue that a developer can work on.

**Testing agent: follow a fix through to verification.** After code merges, it keeps the pending checks. Once a build containing the fix is testable, it runs automated checks and brings together the team's hands-on results.

![Shared records, agents, and team members hand work off: logs lead to investigation, developers fix the issue, and a testing agent waits for a testable build before combining automated and hands-on checks.](assets/team-handoff-en.png)

**The team can see who is following up, what has changed, and what is needed next.** Findings and next steps survive a handoff. Work waiting for a deployment or reply resumes when results arrive.

Scheduled checks can also identify which fixes are ready to test, which are still waiting, and which need someone to retest them.

*The diagram combines two handoffs from the team's account, not one measured end-to-end run. Scheduled checks are a configuration example. Logs, GitHub, and release systems require integration and access permissions.*

## Start with your first task

Install Holon, configure a model provider, and start the background service. Complete one concrete task before adding ongoing responsibilities.

**1. Create from a template.** Choose an Agent Template to reuse a role and its working methods.

**2. Define responsibilities.** State what it owns, what to deliver, and when to ask you. Choose skills and the files and systems it may access.

**3. Assign a task.** Provide the goal, materials, and completion criteria. Review progress, add context, and make decisions when needed.

**4. Add ongoing follow-up.** Receive business messages through Webhooks or adapters, or schedule regular checks. Define which changes need attention and when to involve you.

![The English Holon Web workbench: choose an agent, read updates and progress, inspect plans and waiting reasons, and add information.](assets/web-gui-review-en.png)

*Workbench shown with demo data. Choose an agent on the left, read updates in the center, inspect the plan and waiting reason on the right, and add input below. A terminal interface connects to the same service.*

### Choose how to connect

**Web and terminal.** The binary includes the Web interface. A TUI connects to the same service and existing agents and work; CLI and HTTP API access support automation.

**Mobile clients.** iOS and Android clients connect to a running Holon host; they do not run its background service on the phone. Configure connectivity and authentication for remote access. Beta options:

- [iOS · TestFlight](https://testflight.apple.com/join/XkTfN12S)
- [Android · Join testing](https://groups.google.com/g/holon-pioneers/c/Cgs_KEAjdN0): Google Group testing instructions; access may require membership or permission.
- [Android APK downloads](https://github.com/holon-run/holon/releases): choose an APK from a release; direct installation requires allowing that installation source.

Configure a model, tools, and workspace, and keep the host and background service running for ongoing follow-up. A shared deployment does not imply strong sandbox isolation between assistants. Set file, tool, and access permissions explicitly.

**Get started:** [Installation and quick start](../website/getting-started/README.md) · [Source code](https://github.com/holon-run/holon) · [Documentation](https://holon.run)

**Questions and feedback:** [hello@holon.run](mailto:hello@holon.run)
