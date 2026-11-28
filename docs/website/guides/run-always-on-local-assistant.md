---
title: Run an always-on AI assistant on your own computer
summary: "Bridge a local model you already chat with, such as Ollama or vLLM, to an assistant that keeps working after you close the chat window: durable WorkItems, scheduled wakeups, and results you verify from any device."
order: 65
---

# Run an always-on AI assistant on your own computer

You already run a local model—Ollama, vLLM, or another OpenAI-compatible
endpoint—and chat with it in a terminal or chat window. This guide covers the
step after that: what is still missing before the model can act as an
assistant that keeps working while you are away, and how to close the gap with
Holon without giving up the local model.

The gap is not the model. It is everything around it: a place where a task
outlives the chat window, a way for the agent to wake on a schedule instead of
only when you press Enter, and a way to check results from another device
after you leave the desk. The sections below cover prerequisites, the concrete
differences, what the phone client can and cannot do, and an acceptance test
you run yourself.

## Before you start

- A local model service the host can reach, such as Ollama at
  `http://127.0.0.1:11434` or a vLLM / OpenAI-compatible endpoint. For a
  strict local-only boundary, start from
  [Run private agents offline](./run-offline-private-agent.md).
- A host that stays on while unattended work runs: a desktop, mini PC, or
  server. If the daemon runs on the laptop you carry with you, nothing runs
  while it sleeps—Holon does not wake a suspended host.
- A running Holon daemon with the [Web GUI](./use-web-gui.md) open and a
  [configured model route](./configure-models.md).
- Optional: a paired [iOS](./connect-ios-client.md) or
  [Android](./connect-android-client.md) client, with a network path to the
  daemon from outside your desk network (see
  [Connect to a remote runtime](./connect-remote-runtime.md)).

## Step 1: Confirm what your local setup already gives you

Run two checks before handing over real work.

First, confirm the model service answers on the host:

```bash
curl http://127.0.0.1:11434/api/tags
```

For Ollama this returns the installed model list. If it fails, fix the model
service first; no assistant layer can compensate for an unreachable model.

Second, route the agent to the local model and make the fallback policy
explicit:

```bash
holon config set model.default "ollama@default/your-model"
holon config providers list
```

If cloud provider credentials exist on the host and fallback stays enabled, a
slow or failed local call can fail over to a cloud provider silently. Disable
it if that is not what you want:

```bash
holon config set runtime.disable_provider_fallback true
```

Then verify what actually answered, in conversation:

> "Run a routing self-check without changing any files: report your current
> `model.requested_model`, `model.active_model`, and `model.fallback_active`."

`model.active_model` should match your local route, and
`model.fallback_active` should be `false` when fallback is disabled. For
evidence on which local models finish multi-turn agent work, see
[the local model benchmark](../blog/local-model-agent-benchmark.md).

## Step 2: Give the work somewhere to outlive the chat window

In the Web GUI chat, hand the agent a goal with acceptance criteria and ask it
to track the work in a dedicated WorkItem:

> "Track this in a dedicated WorkItem: <your goal, with acceptance criteria>.
> Write the plan first, wait for my review, then execute in the background and
> complete with a report."

Compared with the chat window you already have, four things change:

- **Who keeps executing after you leave.** The daemon owns background command
  tasks; the agent sleeps between steps without spending tokens, and nothing
  depends on your terminal or browser tab staying open.
- **Where the task state lives.** The WorkItem, its plan, and its checklist
  are persisted on the host and survive daemon restarts and host reboots.
- **Where results are checked.** The same WorkItem and completion brief are
  visible from the Web GUI and from the mobile client; they are not tied to
  the session that started the work.
- **What happens on failure.** Only the failed milestone is retried from the
  recorded checklist; replaying the whole conversation is not the recovery
  path.

[Run long-lived tasks](./run-long-lived-task.md) walks through the full
workflow. Two boundaries stated plainly:

- This is not a 24/7 availability claim. Work continues only while the host
  is on and the daemon is running.
- Recurring work can wake on a schedule with
  [timers and scheduled checks](./schedule-agent-work.md); a suspended host
  still runs nothing.

## Step 3: Know what the phone client can and cannot do

The mobile client is a window into the same runtime, not a second brain.
Agents run on your host; the phone renders conversations, briefs, and files,
and delivers your input back. Check this table before relying on it away from
your desk:

| What you want to do | Works offline? | What to check when it fails |
| --- | --- | --- |
| Read a conversation that already synced | Yes, within the app's cache limits | Cached text is a snapshot; it does not prove the runtime is reachable |
| Open a file or artifact referenced in a brief | No; it needs a live connection | Cached briefs do not cache linked files. Restore the connection first |
| Send a message, approve a plan, stop a run | No; it needs the daemon reachable | The request may sit in a sending queue (iOS) or the offline outbox (Android). Reconnect and inspect it before resubmitting |
| Share into an agent, or write a prompt while offline | Staged, not executed | iOS saves Share-inbox items and Android queues the outbox; dispatch happens after reconnecting. Review the queue before resubmitting |
| Start or stop the host daemon; daemon-level diagnostics | Not provided by the client | Do this on the host. The client's Diagnostics report covers connection state and client versions only |

When you check a result, distinguish three states: **the conversation
synced** (you can read it), **the artifact opens** (the referenced file
resolves on the runtime), and **the completion brief exists** (the agent
reported a result, not just activity). For pairing and platform details, see
[Connect with the iOS client](./connect-ios-client.md) or
[Connect with the Android client](./connect-android-client.md).

## Step 4: Run your own acceptance test

Pick one real task that spans at least a few hours and has a verifiable
deliverable, such as a slow test suite or a batch of files to process. Then
leave:

1. In the Web GUI chat, define the goal with acceptance criteria, ask the
   agent to track it in a WorkItem, execute in the background, and complete
   with a report.
2. Close the browser tab and walk away from the desk, leaving the daemon host
   on.
3. From your phone or another machine, open the client and check the work.
4. Optionally, restart the daemon once mid-task and confirm the WorkItem is
   still there, with its state intact, when it comes back.

| Check | Passes when |
| --- | --- |
| The task survived your absence | The WorkItem shows the same plan and checklist, and progress advanced while you were away |
| The result is real | An artifact or file reference resolves and opens from the other device |
| Completion is reported | A completion brief exists and links the changed files or outputs |
| Recovery is bounded | After a daemon restart, the WorkItem state is intact and only uncompleted milestones rerun |

When it does not pass, it is almost always one of three layers:

- **The host went to sleep.** Nothing runs while suspended. Fix the host power
  settings; the WorkItem state is preserved and work resumes.
- **The local model timed out or errored.** Check `model.fallback_active` in
  the agent's self-check: with fallback enabled and cloud credentials
  present, the run may have failed over to a cloud provider. Retry the failed
  milestone only.
- **The phone cannot reach the daemon.** A network-path problem: local-network
  permission, tailnet, or reverse proxy. See
  [Connect to a remote runtime](./connect-remote-runtime.md) and
  [Troubleshooting](./troubleshooting.md).

## See also

- [Run long-lived tasks](./run-long-lived-task.md) — the full unattended workflow, including recovery after interruptions.
- [Run private agents offline](./run-offline-private-agent.md) — locking models, network, and tools to local-only.
- [Schedule recurring agent work](./schedule-agent-work.md) — timers and calendar wakeups.
- [Connect with the iOS client](./connect-ios-client.md) and [Connect with the Android client](./connect-android-client.md).
- [Can Local LLMs Complete Multi-Turn Coding Agent Tasks? Benchmarking GGUF, MoE Turn Bloat, and Prompt Caching](../blog/local-model-agent-benchmark.md) — evidence for choosing a local model.
