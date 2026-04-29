# AgentInbox Dogfood Runbook: GitHub-Backed Flow

This runbook documents the real GitHub-backed AgentInbox workflow used in Holon dogfooding. It distinguishes fixture validation from real GitHub validation and covers the concrete steps for creating callbacks, subscriptions, waiting for review events, and repairing PRs.

## Overview

The AgentInbox dogfood flow uses GitHub as a real event source to notify Holon agents about PR review and comment activity. When a reviewer requests changes or adds comments, the agent wakes, reads the inbox, fetches the actual review content, and can repair the PR.

## Key Distinction: Fixture vs. Real GitHub

### Fixture Validation (Development/Testing)

- Uses `agentinbox source add fixture github`
- Simulates GitHub events locally
- Events are generated manually via CLI
- No actual GitHub integration required
- Suitable for:
  - Local development and testing
  - CI/CD pipeline validation
  - Quick iteration without GitHub rate limits

### Real GitHub Validation (Production Dogfooding)

- Uses real GitHub webhook events
- Requires GitHub App or OAuth installation
- Events are delivered by GitHub in real-time
- Must handle GitHub rate limits and authentication
- Required for:
  - Actual dogfooding on real PRs
  - Production agent workflows
  - Real-world feedback loops

This runbook covers the **Real GitHub** flow.

## Prerequisites

1. AgentInbox CLI installed and configured
2. GitHub source configured for real events (not fixture)
3. Holon agent with callback and inbox access
4. Active pull request to monitor

## Step-by-Step Workflow

### 1. Add Real GitHub Source

Configure the real GitHub source (not fixture):

```bash
agentinbox source add github --home ~/.agentinbox
```

This returns a `sourceId` like `src_github_REAL`.

**Note**: If you've previously added only the fixture source, you'll need to add the real GitHub source separately.

### 2. Create a Wake-Only Callback

Use Holon's `CreateExternalTrigger` tool within your agent to create a callback for the specific PR:

```json
{
  "summary": "GitHub PR #33 review wake",
  "source": "github",
  "resource": "33",
  "condition": "review or comment activity on PR #33",
  "delivery_mode": "wake_only"
}
```

This returns:
- `waiting_intent_id`: Unique identifier for this callback
- `trigger_url`: URL like `http://127.0.0.1:7878/callbacks/wake/TOKEN`

### 3. Create AgentInbox Subscription

Subscribe the agent to receive GitHub events for the specific PR:

```bash
agentinbox subscription add <agent_id> src_github_REAL \
  --match-json '{"number": 33}' \
  --activation-target http://127.0.0.1:7878/callbacks/wake/TOKEN \
  --activation-mode activation_only \
  --home ~/.agentinbox
```

Replace:
- `<agent_id>` with your actual agent ID
- `TOKEN` with the token from your trigger URL
- `33` with the actual PR number

**Important Parameters:**
- `--match-json`: Filters events to only the specified PR number
- `--activation-target`: Your agent's wake-only trigger URL
- `--activation-mode activation_only`: Wakes the agent without queuing the full payload

### 4. Sleep and Wait for GitHub Events

Call `Sleep` in your agent to enter wait mode:

```json
{
  "reason": "Waiting for GitHub review/comment events on PR #33"
}
```

The agent will sleep until a GitHub event occurs.

### 5. Agent Wakes with Activation Context

When a reviewer:
- Requests changes on the PR
- Adds a review comment
- Submits a general review

The agent wakes with activation context:
- `source`: "github"
- `resource`: PR number (e.g., "33")
- `reason`: Type of activity (e.g., "review_requested_changes")
- Full webhook content available via inbox

### 6. Read the Inbox

After waking, read the actual GitHub event details:

```bash
# List all inbox entries for this agent
agentinbox inbox list --home ~/.agentinbox

# Read the specific inbox entry
agentinbox inbox read inbox_<agent_id> --home ~/.agentinbox
```

The inbox contains:
- Full GitHub webhook payload
- Review comments and suggested changes
- Author and timestamp information
- Links to the PR and specific review

### 7. Repair the PR

Based on the review feedback, use Holon's tools to repair the PR:

1. **Read the relevant files** mentioned in the review
2. **Apply fixes** using `ApplyPatch`
3. **Verify changes** with appropriate tests or checks
4. **Commit and push** the repairs

Example flow after waking:

```json
{
  "file_path": "docs/agentinbox-dogfood-runbook.md"
}
```

Then apply edits based on the review comments.

### 8. Continue Iteration

After pushing fixes:
- The agent can sleep again to wait for additional feedback
- Reviewers can add more comments or request more changes
- The cycle repeats until the PR is approved

## Event Types Supported

AgentInbox currently supports these GitHub events:

✅ **Supported:**
- Review comments
- Review submissions (approved, changes requested, commented)
- Issue comments

❌ **Not Currently Supported:**
- CI check-run events
- Status updates
- Pull request state changes (opened, closed, merged)

## Troubleshooting

### No Events Received

1. Verify the GitHub source is real (not fixture):
   ```bash
   agentinbox source list --home ~/.agentinbox
   ```

2. Check the subscription exists and is active:
   ```bash
   agentinbox subscription list --home ~/.agentinbox
   ```

3. Verify the trigger URL is correct and the agent is sleeping

4. Check GitHub App installation has permissions for the repository

### Inbox Empty After Wake

1. Ensure the agent waited for the wake (not a timeout)
2. Check that `--activation-mode activation_only` was used
3. Verify the inbox read command uses the correct agent ID

### Events Not Matching Expected PR

1. Check the `--match-json` filter in the subscription
2. Verify the PR number in the filter matches your actual PR
3. Ensure the GitHub webhook is properly configured to send PR events

## Example: Complete Dogfood Session

Here's a complete example of using this runbook:

1. **Setup source** (one-time):
   ```bash
   agentinbox source add github --home ~/.agentinbox
   ```

2. **Create callback** (in agent):
   ```json
   {
     "summary": "PR #33 review wake",
     "source": "github",
     "condition": "activity on PR #33",
     "delivery_mode": "wake_only"
   }
   ```

3. **Subscribe** (replace `agent_id` and `TOKEN`):
   ```bash
   agentinbox subscription add dogfood-agentinbox-33 src_github_REAL \
     --match-json '{"number": 33}' \
     --activation-target http://127.0.0.1:7878/callbacks/wake/TOKEN \
     --activation-mode activation_only \
     --home ~/.agentinbox
   ```

4. **Sleep and wait**:
   ```json
   {
     "reason": "Waiting for review feedback on PR #33"
   }
   ```

5. **Wake and read inbox** when review arrives:
   ```bash
   agentinbox inbox read inbox_dogfood-agentinbox-33 --home ~/.agentinbox
   ```

6. **Repair and iterate** based on feedback

## Cleanup

When the dogfood session is complete:

1. **Cancel the waiting intent** returned by `CreateExternalTrigger`:
   ```json
   {
     "waiting_intent_id": "<waiting_intent_id_from_CreateExternalTrigger>"
   }
   ```

   This uses the `CancelExternalTrigger` tool with the `waiting_intent_id` value from step 2.

2. **Remove the subscription**:
   ```bash
   agentinbox subscription remove <subscription_id> --home ~/.agentinbox
   ```

3. **Exit the worktree** (if used):
   ```json
   {
     "action": "keep"
   }
   ```

## Related Documentation

- [AgentInbox Callback Integration](./agentinbox-callback-integration.md)
- [AgentInbox Wake-Only Quickstart](./agentinbox-wake-only-quickstart.md)
- [Callback Capability and Providerless Ingress](./callback-capability-and-providerless-ingress.md)
