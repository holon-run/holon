---
title: Troubleshoot a Holon task
summary: Identify the failing layer of a silent, stalled, or unsuccessful task without duplicating work.
order: 260
---

# Troubleshoot a Holon task

Start with read-only checks. Record the agent ID, WorkItem ID, task ID, and the
last observed result before retrying or restarting. A silent client can reflect
a wait, a bounded observation timeout, or a disconnected connection while the
daemon is still working.

## Before you start

Use the runtime and agent that own the work. Replace `builder`, `WORK_ITEM_ID`,
and `TASK_ID` below with actual identifiers:

```bash
holon daemon status
holon agent status builder
holon work-item list --agent builder
holon tail --agent builder --limit 10
```

If the objective already exists, inspect it instead of submitting it again.
[Observe agent work](observe-agent-work.md) explains how to distinguish progress,
waiting reports, and the final result.

## Agent Issues

### Agent not responding

```bash
holon work-item get WORK_ITEM_ID --agent builder
holon task list --agent builder
holon task status TASK_ID --agent builder
```

| Observation | Next action |
|---|---|
| Waiting for operator | Read the question and answer through the operator channel; keep the same WorkItem |
| Waiting for a task | Inspect that task's state and output; do not start a duplicate command |
| Waiting for an external event | Check the external source, callback delivery, and recorded wait |
| Waiting for a timer | Check the timer ID, owner, next fire time, and cancelled/completed state |
| Yielded WorkItem | Inspect the item it yielded to; do not clear unrelated blockers |
| Runnable work but no progress | Check agent lifecycle, provider errors, and recent daemon logs |
| Completed work | Retrieve its `result_brief_id` and inspect the report and artifacts |

A matching wake can resume an inspection while an independent business blocker
remains. A stopped agent or completed owner cannot be revived by replaying a
stale task result. If a callback is missing, follow
[Connect external events](connect-external-events.md#if-the-agent-does-not-resume).

### Agent uses wrong model

```bash
holon agent model get builder
holon config get model.default
holon config models list
```

Check for an intentional per-agent override before changing it. If you want the
agent to inherit the global default, explicitly clear that override:

```bash
holon agent model clear builder
```

Verify the selection on the next authorized request. Do not run another business
action solely to test whether a model change worked.

## Daemon Issues

### Daemon won't start

```bash
holon daemon status
holon daemon logs
```

Look for the actual startup error: an existing process, listener conflict,
invalid configuration, or a filesystem access failure. Check the relevant
setting before changing ports or deleting state. Preserve logs before any
restart.

### Daemon status shows "stopped" after starting

Inspect the logs for the exit cause and confirm that you are using the intended
`HOLON_HOME`. Check disk space and access to that directory if the error points
to storage. Once you have corrected the cause, start the daemon and re-read the
existing WorkItem and task states. Durable records do not guarantee that a
command process survived the stop.

## Credential & Model Issues

### "No available model" or credential errors

```bash
holon config doctor
holon config providers list
holon config models list
```

Use the reported provider/profile error to fix the missing configuration.
[Configure models](configure-models.md) covers setup. Do not switch providers
blindly when the failure may be a timeout or a rate limit.

### API key not recognized

```bash
holon config credentials list
holon config providers get PROVIDER_ID
```

Replace `PROVIDER_ID` with the configured provider. Check that its credential
profile matches the stored profile. For environment-based credentials, test
presence without printing the value:

```bash
if [ -n "${DEEPSEEK_API_KEY:-}" ]; then
  printf 'Credential variable is set in this shell\n'
else
  printf 'Credential variable is absent from this shell\n'
fi
```

Use the variable required by your provider. A variable set in a new shell does
not prove that the already running daemon inherited it. Prefer the credential
store for managed configuration and follow the provider's documented reload or
restart requirements.

### Model returns errors or unexpected behavior

Inspect the run's provider error, availability diagnostics, and retry outcome.
A failed model call is different from a failed shell command. Preserve the
message/turn ID and a redacted error before retrying. If a turn may already have
published a PR or comment, inspect that side effect first.

## Configuration Issues

### Config changes not taking effect

```bash
holon config get CONFIG_KEY
holon config schema
```

Check the exact key and scope, including agent overrides. If you edited the
configuration file directly, `holon config list` can reveal parse errors, but
review its output privately rather than posting a full configuration dump.
Restart only for settings that require it, after considering active work.

### Config reset to defaults

Check whether your shell and daemon use the same Holon home:

```bash
printf '%s\n' "${HOLON_HOME:-<default home>}"
```

A different home can select different configuration and agent state. Locate the
intended home instead of copying or overwriting runtime databases.

## TUI Issues

### TUI display is garbled

Try a display-only change:

```bash
holon tui --no-alt-screen
```

If that resolves it, persist the preference with
`holon config set tui.alternate_screen never`.

### TUI cannot connect to daemon

Check daemon status, connection address, and authentication. For remote access,
use the intended server and a protected token file:

```bash
holon tui --connect https://your-server:8787 --token-file /path/to/control-token
```

A connection failure does not mean the underlying objective stopped. Inspect the
same agent after reconnecting before submitting another request.

## Logs & Diagnostics

### Viewing agent task output

```bash
holon task output TASK_ID --agent builder
holon transcript --agent builder --limit 30
```

Use the task's returned output metadata and artifact locations instead of
assuming a fixed log filename. Include the command exit status or cancellation
reason when diagnosing a failure.

### Full system diagnostic

`holon config doctor` reports configuration and provider availability. Capture
only the relevant redacted section; it is not a substitute for the task's own
error or test output.

### Checking runtime configuration

Use `holon config get CONFIG_KEY` for a specific suspected setting. Keep full
configuration, transcripts, tokens, and callback capability URLs out of public
bug reports.

## Runtime Performance Diagnostics

### Retrieving metrics

For a slow runtime, capture the protected performance snapshot using the actual
control address and token:

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/performance
```

### Metric groups

Group and field contracts are in the
[Observability reference](../reference/observability.md#runtime-performance-snapshots).
Compare snapshots from the same daemon lifetime rather than treating cumulative
counts as the duration of one task.

### Common diagnosis patterns

- High tool time: inspect the affected command and its output.
- Frequent provider retries: inspect provider errors and connectivity.
- Growing context-build time: inspect the affected turn's context diagnostics.
- High idle-poll count: check whether any work is actually runnable.

Use `holon debug trace --search MESSAGE_OR_TURN_ID` for a retained trace. A
missing trace may reflect sampling or retention. To diagnose collector or
metrics export, follow [Configure observability](configure-observability.md).

### Resetting metrics

Process metrics reset on daemon restart. Capture evidence first and avoid
restarting solely to zero counters; before/after snapshots usually preserve more
useful information without interrupting work.

## When to stop and report a blocker

If the cause remains unknown, report the expected and observed behavior, runtime
version, relevant IDs, redacted error, and checks already performed. State which
work remains incomplete. Do not declare success because a retry was accepted,
and do not erase state to make the symptom disappear.

## See Also

- [Observe agent work](observe-agent-work.md)
- [Run a long-lived task](run-long-lived-task.md)
- [Configuration reference](../reference/configuration.md)
