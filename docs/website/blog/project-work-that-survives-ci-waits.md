---
title: "From Issue to Release: Project Work That Survives CI Waits and Sessions"
summary: "What one anonymized team deployment shows about continuous agent work—and what it does not prove."
order: 20
---

# From Issue to Release: Project Work That Survives CI Waits and Sessions

> **Evidence scope:** This article describes one anonymized, self-hosted engineering-team deployment. The figures come from a sanitized read-only snapshot captured on September 7, 2026. They show sustained use in one environment—not a benchmark, a productivity claim, or a promise of equivalent results. Final publication of the deployment description and each figure requires operator and team approval.

Software delivery is not one uninterrupted task. It is a chain of responsibilities separated by systems, people, and waiting:

- an issue needs investigation;
- a pull request needs review;
- CI needs time to finish;
- a deployment needs verification;
- a release may need explicit approval;
- a new report can reopen what appeared to be finished.

When the state of that chain lives in personal terminals and temporary AI sessions, the team repeatedly reconstructs the same context: What was checked? What are we waiting for? Who can decide? Which result should wake the work again?

One small engineering team is using Holon to test a different model: keep the responsibility in named, long-lived agents and explicit WorkItems on infrastructure the team controls.

## A team-owned runtime, not a collection of personal chats

The deployment runs on a team-controlled Linux host. Authenticated members enter the same Holon runtime through OIDC. Agent Homes, WorkItems, tasks, waits, and execution evidence persist on the host, alongside access to the repositories and toolchains the team already uses.

The active agents have stable responsibilities such as:

- issue and user-feedback investigation;
- pull-request review and merge-readiness tracking;
- fix verification and deployment classification;
- release coordination;
- runtime and application operations;
- project and product coordination.

The important choice is not the number of agents. It is that team members can address a stable role—reviewer, tester, release coordinator, operations—instead of searching for the chat window that previously handled the problem.

## What a delivery path looks like

A typical engineering concern can move through several roles and external systems:

```text
Issue, pull request, or external report
  ↓
Investigator verifies the report and gathers context
  ↓
Reviewer checks the change and merge conditions
  ↓
WorkItem waits for CI, deployment, feedback, or a human decision
  ↓
Tester classifies code, CI, deployment, and device validation
  ↓
Release coordinator prepares the next authorized delivery step
  ↓
Operations continues observing the service or deployment state
```

No single agent needs to pretend it owns the entire process. Each role keeps its own responsibility and authority boundary. The continuity comes from explicit work state and events connecting the stages.

When CI has not finished, the reviewer should not poll forever or declare the work complete. The WorkItem records why progress cannot continue and what should wake it. When a task result, external event, or operator input arrives, the runtime can return the agent to the relevant work.

That is a small state transition, but it changes the operating model: waiting is no longer where responsibility disappears.

## A snapshot of sustained use

The September 7, 2026 snapshot recorded the following activity in this one deployment:

| Observed value | What it can support | What it cannot prove |
|---|---|---|
| 14 active long-lived public agents | The team uses stable, named agent roles | That more agents create more value |
| 5 enabled OIDC users and at least 4 operator identities with recorded input | Multiple people use the shared runtime | Frictionless handoff between people |
| 1,663 WorkItems since June 26, 2026 | Work is represented beyond individual sessions | That every WorkItem produced a successful business outcome |
| 1,627 completed and 36 open WorkItems | The lifecycle is actively used for completion and ongoing work | A measured improvement over the previous process |
| 10,667 explicit `WaitFor` calls | Waiting is a primary workflow state, not an edge feature | That every wait was necessary or correctly configured |
| 28,702 external-trigger deliveries | A large amount of work is event-driven | That trigger volume equals productivity |
| 23,081 agent turns and 187,875 tool executions | Agents repeatedly act in real workflows and tool environments | Quality, time saved, or ROI by itself |

At the moment of the snapshot, 13 of the 14 long-lived agents were asleep and one was awake or running. There were still 29 active wait conditions.

This is a useful correction to the phrase “always-on agent.” The responsibilities were continuously available; the models and processes were not continuously busy. Most agents were sleeping until something made their work actionable.

## Evidence of work crossing agent roles

A heuristic review of recent WorkItem objectives found 234 distinct issue or pull-request identifiers. Seventy-six appeared in work handled by two or more agent roles—about 32 percent of the identifiers found by that method.

Common role combinations included reviewer plus investigator, reviewer plus tester, and chains involving reviewer, tester, investigator, or release coordination.

This is not a formal process audit. The identifiers were inferred from WorkItem text and may include classification errors. It does, however, support a narrower claim: the instance is not simply hosting 14 unrelated chatbots. Multiple specialized agents are acting around the same engineering objects at different stages.

## Where humans remain in control

The deployment does not treat persistence as permission.

Agents can inspect state, run checks, prepare changes, wait for results, and summarize evidence. Irreversible or policy-sensitive actions can still require a human decision—for example:

- approving a release;
- merging a change when policy requires approval;
- accepting a security or operational risk;
- publishing external communication;
- changing credentials, access, or production policy.

A durable runtime should make these boundaries easier to hold. The agent can reach the decision point, explain what is known, and wait for an authorized operator rather than losing the work or crossing the boundary silently.

## What this deployment proves

The current evidence supports four conclusions.

### Work can remain continuous when sessions are not

The combination of persistent WorkItems, explicit waits, external triggers, and repeated tool execution shows a workflow that crosses events and terminal sessions. The useful state is not limited to one transcript.

### Agent roles can become stable team responsibilities

The team has assigned recurring engineering responsibilities to named agents. Those roles have different tools, completion standards, and waiting conditions.

### Real environments are part of the value

The agents operate around real repositories, GitHub, CI, builds, deployments, and host tooling. Their work is inspectable through commands, task results, state transitions, and briefs—not only generated prose.

### Event-driven sleeping is more useful than continuous polling

The snapshot shows many persistent responsibilities with very little simultaneous execution. The runtime keeps the work available and wakes the relevant agent when progress becomes possible.

## What it does not prove yet

The same evidence also leaves important questions unanswered.

### Cross-person handoff has not been demonstrated end to end

Multiple people use the instance, but the snapshot cannot reliably reconstruct whether person A started a WorkItem and person B later understood and continued it without an oral handoff. That requires a deliberate, instrumented test.

### Usage volume is not an outcome metric

WorkItem, turn, tool-call, and trigger counts show intensity of use. They do not tell us how many hours were saved, whether defects decreased, or whether releases became faster. Those claims require comparative evidence or a verified team account.

### One team is not general validation

This is a self-hosted deployment used by one engineering team. A second team must still show that the workflow can be adopted without the same history, infrastructure knowledge, or project context.

### Long-lived state has an operating cost

At the snapshot, the Holon data directory was approximately 9.4 GB, the main SQLite database approximately 6.68 GB, and the Holon process resident memory approximately 2.24 GB. The task history included completed, failed, cancelled, and interrupted tasks.

These figures are not presented as a performance benchmark. They show that retention, log rotation, backups, capacity alerts, failure classification, and recovery are part of the product problem. Long-lived work accumulates evidence—and evidence needs an operating policy.

## The next validation

The most useful next experiment is a controlled cross-person handoff:

1. person A starts a real WorkItem;
2. the agent works until it must wait for CI, deployment, or a human decision;
3. person A stops participating;
4. person B uses only the recorded WorkItem state and evidence;
5. person B responds or takes over;
6. the agent resumes the correct work and produces a verifiable result.

The test should measure how long person B needs to understand the state, what questions remain, whether the original person must restate context, and whether the agent stays inside its authority boundary.

That experiment is more important than adding another agent to the dashboard.

## The lesson so far

The strongest result from this deployment is not “a team runs 14 agents.” It is this:

> When CI is still running, deployment is incomplete, or the original operator has left the terminal, the work can remain attached to a named responsibility and an explicit WorkItem until the right event arrives.

Holon is still early, and this is still one deployment. But the work is no longer only a product concept. It is operating across real issues, pull requests, tools, waits, and team roles—and exposing the next problems that a long-lived agent runtime must solve.

**Next:** Read the [Durable Agent Workflow guide](/guides/durable-agent-workflow), or open a GitHub discussion if your team wants to reproduce this workflow with a bounded, measurable responsibility.
