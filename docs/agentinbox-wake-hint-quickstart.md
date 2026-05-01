# AgentInbox Quickstart: Wake-Hint Workflow

This guide shows how to use AgentInbox callbacks for wake-hint notifications in Holon.

## When to Use Wake Hint

Use `delivery_mode=wake_hint` when:
- An external system only needs to signal that something changed
- The agent will fetch its own updates via tool calls
- You want minimal overhead and don't need the full event payload in the queue

## End-to-End Workflow

### 1. Add the GitHub source (fixture)

```bash
agentinbox source add fixture github --home ~/.agentinbox
```

This returns a `sourceId` like `src_fixture_github`.

### 2. Create a wake-hint external trigger

Use Holon's `CreateExternalTrigger` tool within your agent:

```json
{
  "description": "Check AgentInbox for GitHub PR #34 activity",
  "source": "github",
  "scope": "agent",
  "delivery_mode": "wake_hint"
}
```

This returns a `waiting_intent_id` and a `trigger_url` like `http://127.0.0.1:7878/callbacks/wake/TOKEN`.

### 3. Register an AgentInbox subscription

```bash
agentinbox subscription add <agent_id> src_fixture_github \
  --match-json '{"number": 34}' \
  --activation-target http://127.0.0.1:7878/callbacks/wake/TOKEN \
  --activation-mode activation_only \
  --home ~/.agentinbox
```

Replace `<agent_id>` with your agent ID and `TOKEN` with the token from your trigger URL.

### 4. Sleep and wait for activation

Call `Sleep` in your agent. When the GitHub event occurs, the agent wakes with activation context.

### 5. Read the inbox on wake

After waking, read the event details:

```bash
# List inbox entries
agentinbox inbox list --home ~/.agentinbox

# Read specific inbox entry
agentinbox inbox read inbox_<agent_id> --home ~/.agentinbox
```

## What You Get on Wake

The agent receives activation context (not a full message):
- `source`: which system triggered the wake (e.g., "github")
- `resource`: what changed (e.g., PR number, issue ID)
- `reason`: why the wake occurred
- Full webhook content available via `agentinbox inbox read`

Use the inbox commands to fetch the actual event content after waking.

## Comparison: Wake Hint vs. Enqueue Message

| Aspect | Wake Hint | Enqueue Message |
|--------|-----------|-----------------|
| Payload | Activation context only; fetch from inbox | Full event body queued as message |
| Use case | Trigger then fetch on demand | Direct processing without extra tool calls |
| Agent action | Call `agentinbox inbox read` on wake | Process content from message directly |

Choose wake hint when you want the agent to decide when and how to fetch event details, rather than receiving them as a queued message.
