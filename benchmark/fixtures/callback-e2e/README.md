# Callback Capability End-to-End Fixture

This fixture demonstrates the callback capability flow using a mock implementation that mirrors the Holon design.

**Related:** [Issue #16](https://github.com/holon-run/holon/issues/16)

## What This Demonstrates

This is a mock fixture that mirrors the Holon callback capability design for testing and demonstration purposes. It simulates the complete flow without requiring the actual Holon runtime. This fixture shows:

1. **Agent creates a callback capability** - Using the `CreateCallback` tool, the agent gets a secure callback URL
2. **External system registration** - The agent hands that URL to an external system (a CI simulator in this example)
3. **Callback delivery** - When the condition is met (build completes), the external system calls back
4. **Message delivery** - The fixture simulates delivering the event as a message to the agent
5. **Cleanup** - The agent cancels the waiting intent with `CancelWaiting`

## The Scenario

An agent needs to wait for a CI build to complete:

1. Agent asks Holon for a callback capability
2. Agent starts a CI build and registers the callback URL as a webhook
3. CI system builds and, when complete, sends a webhook to the callback URL
4. The fixture simulates delivering the build result as a message to the agent
5. Agent receives the build result and processes it
6. Agent cancels the callback

## Files

- `src/ci-simulator.js` - Simulates an external CI system with webhook support
- `src/agent-scenario.js` - Demonstrates the agent using CreateCallback and waiting for the result
- `test.js` - Test harness that runs the full end-to-end flow
- `package.json` - Minimal Node.js project definition

## Running the Test

```bash
cd benchmark/fixtures/callback-e2e
node test.js
```

The test runs two scenarios:
1. **enqueue_message mode** - Agent receives the full build result as a message payload
2. **wake_only mode** - Agent receives only a wakeup signal, with no payload

## Key Concepts

### Callback Capability

The `CreateCallback` tool returns a `CallbackCapability`:

```javascript
{
  waiting_intent_id: string,
  callback_descriptor_id: string,
  callback_url: string,
  target_agent_id: string,
  delivery_mode: 'wake_only' | 'enqueue_message'
}
```

The `callback_url` is a secure, token-based URL that the agent can give to external systems.

### Delivery Modes

- `enqueue_message` - The callback delivers a full message payload to the agent's queue
- `wake_only` - The callback only wakes the agent; no payload is delivered

This fixture demonstrates **both** modes:
- `enqueue_message` example: Agent waits for CI build and receives the build result
- `wake_only` example: Agent requests a simple notification when an event occurs

### Security

The callback URL includes a cryptographically secure token that:
- Uniquely identifies the callback descriptor
- Cannot be guessed
- Can be revoked via `CancelWaiting`

## Real-World Examples

This pattern applies to many integrations:

- **CI/CD**: Wait for GitHub Actions, CircleCI, or Jenkins builds
- **PR reviews**: React to PR status changes or comment triggers
- **Deployments**: Monitor deployment rollouts or rollback events
- **External workflows**: Wait for approval emails, Slack messages, or third-party service completions

In each case, the callback design pattern avoids provider-specific logic—Holon provides a safe wakeup endpoint, and external systems call it when events complete.
