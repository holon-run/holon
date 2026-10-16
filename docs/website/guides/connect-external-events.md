---
title: Connect external events
summary: Connect a callback, register a wait, and verify that an external event resumes the intended work.
order: 100
---

# Connect external events

Use a callback to notify an agent when CI or another outside system changes.
This walkthrough uses a wake callback: the event tells the agent to inspect the
source, and the agent decides whether the WorkItem can continue.

## Before you start

- Have a running agent with a tracked objective and an external object to check,
  such as a specific CI run.
- Give the agent read access to that source and a rule for what to do on success
  or failure.
- Make the callback reachable from the sending system. A loopback URL works only
  on the runtime host; use your approved remote access setup for a remote sender.
- Store callback URLs as secrets. Disable shell tracing and avoid verbose HTTP
  logging while handling them.
- The operator-managed example below needs `curl`, `jq`, and authenticated
  control-plane access on the runtime host.

## 1. Obtain the callback capability

An agent receives its default ingress capability in runtime context. Ask it to
configure the authorized integration using that capability without printing the
URL into reports or committing it to a repository.

For an operator-managed integration, the authenticated reset endpoint issues a
replacement capability. **Reset revokes the old URL**, so use it only when
provisioning or intentionally rotating that integration:

```bash
BASE=http://127.0.0.1:7878
AGENT=builder
: "${HOLON_CONTROL_TOKEN:?Set the control token securely}"

CAPABILITY=$(curl --fail --silent --show-error \
  -X POST "$BASE/api/control/agents/$AGENT/reset-callback" \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN")
CALLBACK_URL=$(printf '%s' "$CAPABILITY" | jq -er '.trigger_url')
CALLBACK_MODE=$(printf '%s' "$CAPABILITY" | jq -er '.delivery_mode')
unset CAPABILITY
case "$CALLBACK_URL" in
  /*) CALLBACK_URL="$BASE$CALLBACK_URL" ;;
esac
```

The example resolves a returned relative path against the configured runtime
origin. Preserve its path and token; do not replace `/api/callbacks/` with a
guessed route.
Check `CALLBACK_MODE`: this walkthrough expects `wake_hint`. For
`enqueue_message`, follow the payload contract in the
[events reference](../reference/events-and-timers.md#callback-endpoints).
Store the URL in the sender's secret store without echoing it.

## 2. Record what should resume

Ask the agent to save the source identifier and next action in the existing
WorkItem, then call `WaitFor`. This is agent tool input:

```json
{
  "wake": "external",
  "resource": "github:owner/repo#123",
  "reason": "Recheck CI for the recorded commit before reporting acceptance",
  "recheck_after_ms": 600000,
  "delivery": "final"
}
```

Use the actual PR or run identifier in your workflow. The resource identifies
the dependency; it does not subscribe to GitHub. The callback configuration
provides delivery. A fallback recheck deadline helps recover from missed events.

If the runtime requests a final waiting report, the agent supplies it before
yielding. If it returns `continue_turn`, the agent continues instead of reporting
an obsolete wait.

## 3. Send a test event

After the wait is registered, send a wake from the machine that will deliver
real events:

```bash
curl --fail --silent --show-error -X POST "$CALLBACK_URL"
```

The callback URL itself grants the limited callback capability; do not give the
external sender your control-plane bearer token. A successful HTTP response
confirms delivery admission, not CI success or WorkItem completion.

## 4. Verify the resumed work

```bash
holon agent status builder
holon work-item list --agent builder
holon tail --agent builder --include-progress --limit 10
```

Confirm that the agent checked the expected source and commit, recorded its
findings, and either continued or registered another wait. Test a pending or
failed source state too: a wake must not make the agent report success merely
because a callback arrived.

## 5. Retire or rotate the integration

Remove the sender's callback configuration when it is no longer needed. An agent
can revoke an owned trigger with `CancelExternalTrigger` using its returned
trigger ID. A reset replaces the default capability and invalidates the previous
URL. Update the sender's secret and test the replacement after rotation.

Do not revoke a shared default ingress just because one WorkItem completed.
Confirm that no other integration depends on it first.

## If the agent does not resume

- Connection failure: test reachability from the sender, not just localhost.
- Rejected callback: check the active capability and delivery mode; rotate an
  exposed or stale secret instead of weakening authentication.
- Accepted event with no useful progress: inspect the agent's wait and WorkItem.
  A stopped agent or a completed owner cannot be resumed by an old callback.
- Resumed agent still blocked: inspect the business dependency. A wake may resume
  inspection without clearing an independent blocker.

See [Troubleshooting](troubleshooting.md) for task and provider failures.
