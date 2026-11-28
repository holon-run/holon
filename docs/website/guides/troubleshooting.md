---
title: Troubleshoot a stuck or unresponsive agent task
summary: Diagnose silent, stalled, or failed agent tasks in Web GUI conversation first, distinguish active WaitFor pauses from real blockers, inspect TaskStatus and subagents, and resume safely without duplicating work.
order: 260
---

# Troubleshoot a stuck or unresponsive agent task

Use this guide when an agent stops replying, a background command seems stuck, a delegated subagent has not reported back, or a run fails with model, credential, or workspace errors. Start with read-only inspection before resending instructions or restarting the daemon so you do not duplicate side effects or discard active state.

## Applicable scenarios

- A conversation turn goes quiet and you are unsure whether the agent is still running, waiting on a task, waiting on your approval, or stuck.
- A parent agent delegated work to a subagent and appears idle.
- An agent reports a provider error, credential mismatch, or Git worktree conflict.
- The Web GUI, TUI, or CLI cannot reach the Holon daemon.

## Prerequisites

- A running Holon runtime (default Web GUI at `http://127.0.0.1:7878/`) or shell access to the host running `holon`.
- Identify the agent handling the task (for example `main`, `builder`, or `reviewer`) and keep the existing `WorkItem` rather than submitting the same objective again.

## Steps

### 1. Inspect the WorkItem and wait state in conversation

In the Web GUI conversation, ask the agent directly before retrying:

> "Check your open WorkItems, current scheduling state, active `WaitFor` conditions, and running tasks. Are you waiting on a valid dependency or stuck on a blocker?"

You can also open the **Work Items** panel and switch the conversation display level from **Info** to **Verbose** or **Debug** in the [Web GUI](use-web-gui.md) to view recent tool calls and waiting indicators. Match what you observe to the right next step:

| Observed state | What it means | Next action |
|---|---|---|
| `runnable` (actively executing) | The agent is running a model round or tool call | Wait for the active round or inspect recent progress; do not resend the prompt |
| `yielded` | Execution paused by a continuation frame (for example switched to another WorkItem via `PickWorkItem`) | Inspect the active WorkItem it yielded to; do not manually clear unrelated blockers |
| `waiting` (`wake=task_result`) | Zero-token sleep waiting for a background command or subagent task | Inspect that task's `TaskStatus` and output rather than starting a duplicate command |
| `waiting` (`wake=timer` or `wake=external`) | Parked until a timer fires or an external webhook callback arrives | Check the timer schedule or external event source; see [Connect external events](connect-external-events.md#troubleshooting) if a webhook did not resume |
| `waiting_for_operator` (`needs_input` or `wake=operator_input`) | Paused because your decision, approval, or credentials are required | Answer directly in the same conversation so the runtime resumes the same WorkItem |
| `blocked` | Owns an unresolved blocker or failed dependency | Diagnose the blocker at the failing layer before clearing it |
| `completed` (or interrupted historical run) | The WorkItem already settled or stopped | Read its completion brief and artifacts before deciding whether follow-up work is needed |

### 2. Narrow down the failing layer in conversation

If the task is genuinely blocked or failed, ask the agent to inspect one layer at a time:

1. **Background command tasks (`TaskStatus` and output logs):**

   > "Run `TaskStatus` on the task ID, report its lifecycle state and exit status, and read the tail of any available stdout or stderr log path."

   `TaskStatus` reports the exact command state and runtime-confirmed log file paths. Never assume a fixed log path or declare a running command failed without checking its exit status or cancellation reason.

2. **Delegated subagents (`ListSubagents`):**

   > "List your directly supervised subagents with `ListSubagents` and tell me which child task is still running, waiting, or failed."

   `ListSubagents` checks current supervision state and lightweight runtime status without waking an idle child or consuming its task result. In the Web GUI, you can also open the child task detail card and click into the child's conversation directly. See [Delegate complex tasks to subagents](delegate-work.md).

3. **Model routes, credentials, and provider timeouts:**

   > "Check your active model route and the exact provider error from the failed round without switching providers blindly."

   A transient rate limit or upstream timeout is different from a missing credential. Do not switch models mid-task unless the active route is actually misconfigured or unavailable. See [Configure and switch models](configure-models.md).

4. **Workspace and Git worktree conflicts:**

   > "Inspect `GetWorkspaceState` and show whether a dirty, locked, or occupied worktree is blocking edits or cleanup."

   Never force-remove a worktree or discard uncommitted changes until you confirm which agent or branch owns them. See [Work in a project workspace or Git worktree](work-with-workspaces.md).

### 3. Resume cleanly with minimal intervention

Once you have confirmed the root cause:

- **Waiting on your decision:** Reply directly in chat (for example, approve the action or supply the missing constraint). Answering satisfies `operator_input` and resumes the WorkItem cleanly.
- **External blocker fixed:** If you fixed a broken dependency, restored credentials, or resolved a Git conflict out of band and the WorkItem is still marked `blocked`, tell the agent:

  > "Verify that the blocker is resolved, resume this WorkItem (`PickWorkItem` with `clear_blocker=true` and the reason), and continue from the next pending step."

- **Failed command or provider error:** Fix the underlying configuration or command issue first, then ask the agent to retry only the failed verification or step.
- **Side-effect check before retrying:** If a turn failed after calling GitHub, git push, or another external service, ask the agent to verify whether the external action already landed before retrying so you do not post duplicate comments or commits.

## Host-level & CLI diagnostics (when Web GUI is unavailable)

When the daemon will not start, no model is reachable, or you are debugging a headless server over SSH, use these read-only CLI and HTTP checks on the host.

### Daemon won't start or stops immediately

```bash
holon daemon status
holon daemon logs
holon daemon validate-startup
```

Check the actual startup error before changing ports or touching `~/.holon/`: an existing daemon instance, a bound port conflict, invalid config syntax, disk space exhaustion, or a mismatched `HOLON_HOME`:

```bash
printf '%s\n' "${HOLON_HOME:-<default home>}"
```

Do not copy or overwrite runtime databases across different `HOLON_HOME` directories. After fixing the cause and restarting the daemon, re-read open WorkItems and task states—durable WorkItems survive restarts, but an in-flight shell command may have exited when the daemon stopped.

### Upgrading from the retired destructive-operation scheduler

The upgrade removes `ScheduleDestructiveOperation` and its private ledger. It
does not run saved verification commands, retry old operations, or stop jobs
already handed to an external executor. A missing ledger entry does not mean an
operation succeeded or its process exited. If an old dispatch may still be
running, independently check its status in the external service manager before
deciding whether to start a similar operation. Run any authorized daemon restart
from an operator session outside the daemon's own service.

### Agents quarantined after upgrading from a 0.62.0 beta

If a node had already run a 0.62.0 beta, the upgrade could leave historical
turn input evidence drifted. The daemon then quarantined the affected agents
during startup, logged `input evidence canonical projection changed without
qualified writer`, answered their wakes with 503, and held their queued
inputs. v0.62.0 repairs the drifted evidence during startup: upgrade
directly to v0.62.0 or later and restart the daemon.

### "No available model" or credential errors

```bash
holon config doctor
holon config providers list
holon config model-providers list
holon config models list
holon config credentials list
```

Check whether the provider's credential profile matches the stored credential. If using environment variables, verify presence without printing secrets:

```bash
test -n "${DEEPSEEK_API_KEY:-}" && echo "set" || echo "unset"
```

Environment variables exported in a new shell do not affect an already-running daemon; restart the daemon or store credentials in the managed credential store.

If a local credential-free or anonymous-access provider (`ollama`, `vllm`, `magpie`) shows no models, opt it in by declaring `providers.<id>` in `config.json`, referencing it in a configured model route, or running `holon config models refresh --provider <id>`.

### Config changes not taking effect

```bash
holon config get CONFIG_KEY
holon config schema
```

Verify the exact key and whether a per-agent override exists. Keep full `holon config list` dumps, transcripts, tokens, and callback URLs out of public issue reports.

### Read-only CLI task inspection

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
holon task status TASK_ID --agent builder
holon task output TASK_ID --agent builder
holon tail --agent builder --limit 10
holon transcript --agent builder --limit 30
```

### TUI issues

- Garbled terminal display: run `holon tui --no-alt-screen` (persist via `holon config set tui.alternate_screen never`).
- Remote connection failure: verify the daemon address and token file:

  ```bash
  holon tui --connect https://your-server:8787 --token-file /path/to/control-token
  ```

### Runtime performance snapshots and traces

Capture a protected runtime performance snapshot using your control token:

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/performance
```

Compare snapshots within the same daemon lifetime using the metric definitions in the [Observability reference](../reference/observability.md#runtime-performance-snapshots):

- High `turn.tool_execution`: inspect the slow command and its output.
- Frequent provider retries: check upstream errors, rate limits, and network connectivity.
- Growing `turn.context_build`: inspect context size and retained diagnostics.
- High idle-poll counts: verify whether any WorkItem is actually runnable.

Use `holon debug trace --search MESSAGE_OR_TURN_ID` for retained traces, or follow [Configure observability](configure-observability.md) to verify OTLP export. Avoid restarting the daemon solely to reset counters before capturing diagnostics.

### ACP harness agents (`agy acp`)

An `acp`-driver agent runs its model turn inside an external `agy acp`
process. Holon supervises the process and decides its permission requests.

1. Verify the binary with `agy --version`, then log in with `agy acp --login`.
   Without a terminal login, configure `GEMINI_API_KEY` / `GOOGLE_API_KEY`, or
   Vertex ADC via `GOOGLE_APPLICATION_CREDENTIALS`, `GOOGLE_CLOUD_PROJECT`,
   `GOOGLE_CLOUD_LOCATION`, and `AGY_ADC_AUTH`.
2. `-32000` means authentication is missing. Log in or configure credentials;
   Holon settles the turn as a structured runtime error instead of retrying.
3. `-32002` means the persisted session is gone. Holon rebuilds it and records
   `acp_session_rotated`; session-scoped `allow_always` grants are dropped.
4. The default `unattended` permission mode never waits for a person. To answer
   approvals and form questions, set `acp.profiles.<profile>.permission_mode`
   to `interactive` and tune `acp.interaction_timeout_ms`; then use
   `GET/POST /api/control/agents/{agent_id}/acp-interactions` with control
   credentials. Unanswered requests fall back to rejection, and a settled id
   answers `409` (`settled_reason`) while an unknown id answers `404`.
   Interactive mode is stricter than unattended: even in-root requests wait and
   are rejected on timeout. Only `allow_always` is remembered for the session;
   `reject_always` applies to one request.
5. Plan, harness-internal subagent, and usage updates appear as
   `acp_plan_updated`, `acp_subagents_updated`, and `acp_usage_updated` events.
   They are read-only evidence: harness subagents never enter Holon's own
   supervision tree. These events are emitted once per turn when it settles.

See the full operator checklist in the repository's
`docs/local-operator-troubleshooting.md`.

## When to stop and report a blocker

If the cause remains unresolved after these checks, stop retrying and collect:

- The agent ID, WorkItem ID, task ID, and message/turn ID.
- Expected versus observed state and the exact redacted error message.
- Which read-only checks you already ran and any external side effects already confirmed.

## See also

- [Observe, track, and verify agent progress](observe-agent-work.md)
- [Delegate complex tasks to subagents](delegate-work.md)
- [Run unattended long-lived tasks](run-long-lived-task.md)
- [Schedule periodic tasks](schedule-agent-work.md)
- [Configuration reference](../reference/configuration.md)
