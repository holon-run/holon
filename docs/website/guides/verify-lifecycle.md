---
title: "Verify lifecycle guarantees yourself: restart, wake, isolation, and trust"
summary: Reproduce Holon's core runtime claims on a clean install in about 20 minutes — durable work across daemon restarts, zero-token timer and webhook wakes, git worktree isolation, and external-input trust classification.
order: 71
---

# Verify lifecycle guarantees yourself

This guide verifies four runtime claims about Holon on your own machine, with
evidence you can inspect at every step. You will stop the daemon while an agent
is sleeping, restart it, and watch the pending work resume; you will isolate a
change in a managed git worktree; and you will trigger an external webhook wake
and check how the agent classifies it.

| # | Claim | How you verify it |
|---|---|---|
| 1 | Assigned work is durable | Stop and restart the daemon mid-task; the WorkItem, timer, and wait survive |
| 2 | Waiting costs nothing and wakes on time | A one-shot timer fires after the restart and the agent resumes |
| 3 | Parallel edits are isolated | A change lands only in a managed worktree branch; the main checkout stays clean |
| 4 | External input is not operator input | A webhook wake resumes the agent but carries no operator authority |

Everything below was verified against `holon` 0.59.0 on Linux; the same steps
apply on macOS. Screenshots are not needed — every checkpoint ends in a
command whose output is the evidence.

## Before you start

- Complete [Getting started](../getting-started/): Holon installed, a model
  configured via `holon onboard`, and the daemon running
  (`holon daemon start`). The Web GUI is at <http://localhost:7878>.
- About 20 minutes, most of it waiting for timers.
- A scratch directory for a marker file and a tiny demo git repository
  (Checkpoint 3). You can also reuse any local repository you own.

All conversation steps happen in the Web GUI with your `main` agent. CLI
commands are used only to control the daemon and to read evidence.

## Checkpoint 1: work survives a daemon restart

### Arm the task

In the Web GUI, send this to your agent (adjust the marker path):

> Lifecycle demo. Do exactly this, autonomously, without asking questions:
> 1) Create a WorkItem with objective "Lifecycle demo: survive daemon restart
> and wake on timer". 2) Create a one-shot timer that fires 3 minutes from
> now. 3) Register a wait on that timer and yield into zero-token sleep.
> 4) When the timer fires, append exactly one line `timer-woke <UTC timestamp>`
> to /tmp/holon-demo/marker.txt (create it if missing), then complete the
> WorkItem with a short report stating when the timer fired.

The agent creates the WorkItem, the timer, and the wait, then goes dormant.
Confirm all three exist before you interrupt anything:

```bash
holon work-item list --agent main
holon timer list --agent main
```

You should see one open WorkItem and one active timer whose `next_fire_at` is
about three minutes away.

### Stop and restart the daemon while it sleeps

```bash
holon daemon stop
holon daemon status   # control_connectivity: false, desired_running: false
holon daemon start
holon work-item list --agent main   # same WorkItem, still open
holon timer list --agent main       # same timer, still active, same fire time
```

The WorkItem, its plan, the timer, and the registered wait were reloaded from
the local state store; nothing was re-created or lost.

### Watch the wake

Wait until `next_fire_at` passes. Then:

```bash
cat /tmp/holon-demo/marker.txt        # timer-woke 2026-11-19T17:01:11Z
holon work-item list --agent main     # the WorkItem is now completed
```

In the Web GUI you will find a completion brief reporting when the timer fired.
The timestamp in the marker file is written by the agent after the restart —
this is the resume, not a leftover from before.

**What this proves:** assigned objectives are durable records, timers are
durable, and a sleeping agent consumes no model tokens while it waits — the
wake is driven by the runtime, not by polling.

## Checkpoint 2: edits are isolated in a managed worktree

Create a disposable repository:

```bash
mkdir -p /tmp/holon-demo && cd /tmp/holon-demo
git init repo -b main && cd repo
echo "# Demo repo" > README.md && git add . && git commit -m "init"
```

Then send this to your agent:

> Worktree isolation demo. Do exactly this, autonomously: 1) Attach the
> workspace at /tmp/holon-demo/repo. 2) Create a managed Git worktree on
> branch demo-change from main. 3) Inside that worktree ONLY, append the line
> "isolated change by holon demo" to README.md and commit it. 4) Do not
> modify the main checkout. 5) Reply with the worktree path, the commit hash,
> and confirm the main checkout is clean. Leave the worktree for inspection.

When the agent finishes, verify from the shell — do not trust the reply alone:

```bash
cd /tmp/holon-demo/repo
git status --short          # empty: main checkout untouched
git log --oneline -1        # still the initial commit
git worktree list           # a second entry on branch demo-change
git log --oneline -1 demo-change   # the isolated commit
```

**What this proves:** agents can make real commits without touching your
working checkout; worktrees are runtime-managed and removable through
conversation (ask the agent to clean up the worktree when you are done).

## Checkpoint 3: an external wake is a hint, never authority

Ask the agent to arm an external wait:

> External wake demo. Do exactly this, autonomously: 1) Create a WorkItem
> "External wake demo". 2) Register a wait for an external event with resource
> "demo:external-signal" and a 10-minute fallback recheck, then yield. 3)
> Before yielding, print your external wake callback URL and remind me it is
> a capability secret. When you wake: inspect the wake provenance in your
> context, state explicitly whether it came from an operator or an external
> trigger, append one line `external-woke <UTC timestamp>` to
> /tmp/holon-demo/marker.txt, and complete the WorkItem with a short
> provenance report.

The agent replies with a URL like
`/api/callbacks/wake/cb_<long-token>`. Trigger it the way an external system
would:

```bash
curl -X POST "http://localhost:7878/api/callbacks/wake/cb_<your-token>"
```

You get an immediate `200 OK`, the agent wakes, and its completion report
describes the provenance it observed: a wake hint attributed to an external
trigger bound to the callback capability — explicitly **not** an operator
message, carrying no operator authority. The marker file gains its
`external-woke ...` line, and the WorkItem completes.

**Security note:** the callback URL is a capability secret. Anyone holding it
can wake the agent, which is exactly why waking is only a hint: the agent must
verify ground truth with its own tools before acting on any claimed state.

## Interpreting the results

| Observation | It means | It does not mean |
|---|---|---|
| WorkItem and timer survive restart | Objectives are durable local records | Multi-host replication or HA — a Holon daemon is single-host |
| Wake happens at the fire time with no polling | Waits are runtime-scheduled, zero-token | The agent is "running" while asleep; it is parked |
| Change exists only on the worktree branch | Parallel edits are physically isolated | Other agents cannot see the branch; they can if given access |
| External wake is classified as external | Provenance is preserved end-to-end | The webhook sender is authenticated; a wake never proves payload claims |

If a checkpoint does not reproduce, [troubleshoot the task](./troubleshooting.md)
before concluding the claim is false — most failures are setup issues such as
an unreachable model provider or a daemon on a non-default port.

## Cleanup

```bash
holon work-item list --agent main   # demo items show completed
rm -rf /tmp/holon-demo
```

If you ask the agent to remove the demo worktree in conversation, it will use
the managed cleanup path and delete the branch only when merged or explicitly
allowed.

## Next steps

- [Architecture comparison](../concepts/architecture-comparison.md) — how these
  behaviors differ across self-hosted agent runtimes, CLI shells, and cloud bots
- [Run unattended long-lived tasks](./run-long-lived-task.md) — the workflow
  version of Checkpoint 1
- [Connect external events](./connect-external-events.md) — real webhooks from
  GitHub, GitLab, and alerting systems
- [Events, waits, and wakeups](../concepts/events-and-waits.md) — the concepts
  behind every checkpoint in this guide
