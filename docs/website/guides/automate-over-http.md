---
title: Automate Holon over HTTP
summary: Authenticate, submit one tracked objective, follow its state, and retrieve its completion report.
order: 120
---

# Automate Holon over HTTP

This walkthrough submits a WorkItem through the operator control plane, saves
its ID, and retrieves its final report. Use public enqueue or callback routes
for external evidence; they do not confer operator authority.

## Before you start

Have `curl`, `jq`, a configured model, and an existing agent with the intended
workspace. The examples use `builder` and run from an operator shell. Do not
hand a control-plane token to an untrusted webhook sender.

## Starting the Server

For an isolated local server, load a token securely into `HOLON_CONTROL_TOKEN`,
then start the server in a separate terminal:

```bash
: "${HOLON_CONTROL_TOKEN:?Set the control token securely}"
HOLON_CONTROL_TOKEN="$HOLON_CONTROL_TOKEN" holon serve --port 8787
```

If the daemon already serves HTTP, use its address and token instead of starting
a second server. The default address is `127.0.0.1:7878`; this walkthrough uses
port `8787`. Non-loopback binding and `lan`/`tailnet` access require authentication.
Keep local tests on loopback and disable shell tracing around secrets.

## API Conventions

Set these variables in the terminal that will make the requests:

```bash
BASE=http://127.0.0.1:8787
AGENT=builder
: "${HOLON_CONTROL_TOKEN:?Set the control token securely}"

api() {
  curl --fail --silent --show-error \
    -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
    -H 'Content-Type: application/json' "$@"
}

api "$BASE/api/agents/$AGENT/status"
```

Continue only if this returns the intended agent. All routes below include the
`/api` prefix. See the [HTTP reference](../reference/http-control-plane.md) for
exact request and response schemas.

## Examples

### Create an agent

This walkthrough uses an existing agent. If you need a new one, follow
[Create an agent](create-agent.md) and [Work with workspaces](work-with-workspaces.md)
first, then repeat the status check above. The HTTP creation route is documented
in the [HTTP reference](../reference/http-control-plane.md).

### Create a work item

Submit once and save the returned record:

```bash
WORK=$(api -X POST "$BASE/api/control/agents/$AGENT/work-items" \
  -d '{"objective":"Inspect the attached project read-only. Identify its documented test command, confirm where it is defined, and complete this WorkItem with a report. Do not edit files, run tests, or publish anything."}')
WORK_ID=$(printf '%s' "$WORK" | jq -er '.id')
printf 'WorkItem: %s\n' "$WORK_ID"
```

A successful response means the objective was accepted and recorded. It does
not mean that the agent has finished. If the request times out after submission,
list the agent's WorkItems and match the objective before retrying; a blind
retry can create duplicate work.

### Check agent status

Read that exact WorkItem rather than treating a general agent status as its
completion signal:

```bash
api "$BASE/api/agents/$AGENT/work-items/$WORK_ID" \
  | jq '{id, objective, state, plan_status, blocked_by, result_brief_id, result_summary}'
```

For a script, use a bounded observation window. This example checks for up to
one minute and stops early when operator input is required:

```bash
for attempt in $(seq 1 12); do
  WORK=$(api "$BASE/api/agents/$AGENT/work-items/$WORK_ID") || break
  STATE=$(printf '%s' "$WORK" | jq -r '.state')
  PLAN=$(printf '%s' "$WORK" | jq -r '.plan_status')
  [ "$STATE" = completed ] && break
  [ "$PLAN" = needs_input ] && break
  sleep 5
done
printf '%s\n' "$WORK" | jq .
```

A timeout leaves the work in its actual state; do not mark it complete to make
an automation job pass. If it needs a decision, read the latest brief and answer
through an authorized operator channel.

When `state` is `completed`, fetch the linked report:

```bash
BRIEF_ID=$(printf '%s' "$WORK" | jq -er \
  'select(.state == "completed") | .result_brief_id // empty')
api "$BASE/api/agents/$AGENT/briefs/$BRIEF_ID"
```

Run the fetch only if the ID extraction succeeds. Check that the report answers
the objective and identifies its evidence. For tasks that produce files, inspect
the returned artifact locations too.

### Update and complete a work item

Let the executing agent update and complete its own work. Operator-side PATCH
and completion endpoints exist for explicit lifecycle management, but an active
item cannot be completed by an unrelated control request. Completion requires a
non-empty report and acceptance evidence. See the
[WorkItem reference](../reference/work-items.md#core-operations) before using
these mutation routes.

### Send a message to an agent

Use public enqueue for outside information, such as a notification to inspect a
CI run:

```bash
api -X POST "$BASE/api/agents/$AGENT/enqueue" \
  -d '{"text":"The CI run changed; inspect its current status.","priority":"normal","origin":{"kind":"webhook","source":"ci-service"}}'
```

The acceptance response includes a message ID. It does not prove the requested
inspection ran. Public enqueue accepts only `channel` or `webhook` origins,
rejects caller-supplied trust and `interject`, and preserves the external origin.
For operator transport bindings, use the HTTP reference rather than relabeling
an external payload as an operator message.

### Create and cancel a timer

For scheduled follow-up, use [Schedule agent work](schedule-agent-work.md).
It covers the agreed action, timer creation, verification, and cancellation.
The [events reference](../reference/events-and-timers.md) lists the HTTP routes.
A timer's creation is not a wait registration.

### Wake a sleeping agent

Use a callback capability for an external sender, as described in
[Connect external events](connect-external-events.md). A wake does not clear an
independent business blocker or authorize additional actions. The operator wake
endpoint and payload are in the [HTTP reference](../reference/http-control-plane.md).

### List agent tasks

```bash
api "$BASE/api/agents/$AGENT/tasks"
```

Match task IDs and WorkItem ownership before reading a task's output. Task
completion and objective completion are separate checks.

### Get agent transcript

```bash
api "$BASE/api/agents/$AGENT/transcript?limit=30"
```

Use bounded transcript reads when the report and status do not explain a failure.
Redact secrets and unrelated conversation before sharing diagnostic output.

## If the workflow fails

- Authentication failure: check the server mode, token, and target address.
- Agent not found: verify the ID and caller access; do not silently create a
  replacement for a misspelled ID.
- Accepted but unfinished: inspect the WorkItem, recent briefs, and task state.
- Connection lost after a mutation: inspect durable state before retrying.
- Completed without a usable report or artifact: preserve the IDs and report
  the inconsistency; do not infer a successful result from HTTP status alone.

## See Also

- [HTTP control plane reference](../reference/http-control-plane.md)
- [Observe agent work](observe-agent-work.md)
- [Trust boundaries](../concepts/trust-boundaries.md)
