# `holon serve` Webhook Mode

`holon serve` supports webhook mode for consuming GitHub events in real-time via `gh webhook forward`. This enables local development and testing of the controller without setting up external webhook infrastructure.

## API Direction

**Important**: `holon serve` is following a provider-specific ingress path strategy as documented in [RFC-0005](../rfc/0005-serve-api-direction.md). The webhook endpoint uses the provider-specific path `/ingress/github/webhook` to keep ingress separate from the future control-plane API.

See the RFC for details on:
- Provider-specific ingress paths (`/ingress/<provider>/webhook`)
- Codex/OpenAI-style JSON-RPC control plane (future)
- Deferred generic `/v1/events` endpoint

## Prerequisites

1. **GitHub CLI (gh) installed**: The runtime Docker image now includes `gh` CLI and the `gh-webhook` extension.
2. **GitHub authentication**: Run `gh auth login` to authenticate with GitHub.
3. **Repository access**: You need access to the target repository for webhooks.

## Quick Start

### 1. Start `holon serve` in webhook mode

```bash
holon serve --repo holon-run/holon --webhook-port 8080
```

This starts an HTTP server on port 8080 that:
- Listens for webhook POST requests at `/ingress/github/webhook` (new path)
- Exposes JSON-RPC control endpoint at `/rpc`
- Exposes NDJSON notification stream at `/rpc/stream` (`Accept: application/x-ndjson`)
- Provides a health check endpoint at `/health`
- Normalizes incoming GitHub webhook events to EventEnvelope format
- Forwards events to the controller agent

**Note**: The legacy `/webhook` path is still supported for backward compatibility but is deprecated. See [Migration](#migration) below.

### 2. Forward webhooks from GitHub

In a separate terminal, start webhook forwarding:

```bash
gh webhook forward --repo holon-run/holon --events=issues,pull_requests,pull_request_review_comments,issue_comment --port 8080
```

This command:
- Uses `gh webhook forward` (requires the `gh-webhook` extension)
- Forwards specified GitHub events to your local `holon serve` instance
- Requires the `gh-webhook` extension (now installed in runtime image)

### 3. Trigger events

Any GitHub events matching the configured types will be forwarded to your local `holon serve` instance. Events appear in:
- `events.ndjson` - All received events
- `decisions.ndjson` - Forward/skip decisions
- `actions.ndjson` - Controller action results

## Configuration Options

### Command-line flags

- `--repo owner/repo` (required for webhook mode): Repository in owner/repo format
- `--webhook-port PORT`: Enable webhook mode and listen on this port
- `--state-dir DIR`: State directory for cursor/dedupe persistence (default: `.holon/serve-state`)
- `--controller-workspace DIR`: Controller workspace path
- `--log-level LEVEL`: Log level (debug, info, progress, minimal)
- `--runtime-mode MODE`: Runtime mode (choices: `prod` default, `dev`)
  - `prod`: Use bundled agent code (default, CI-safe)
  - `dev`: Overlay local `dist/` for debugging
- `--runtime-dev-agent-source DIR`: Local agent source for `--runtime-mode=dev` (defaults to `./agents/claude` when available)
- `--dry-run`: Log events without running controller

### Example with options

```bash
holon serve \
  --repo holon-run/holon \
  --webhook-port 8080 \
  --state-dir .holon/serve-state \
  --controller-workspace ~/.holon/workspace \
  --log-level debug \
  --runtime-mode dev \
  --runtime-dev-agent-source ./agents/claude
```

## Startup Diagnostics

On startup, `holon serve` writes `${state_dir}/serve-startup-diagnostics.json` and logs the same diagnostic snapshot.

Key fields:
- `role_source`: Always `${agent_home}/ROLE.md` (single source of truth)
- `role_inferred`: Role inferred from `ROLE.md` content (`pm`/`dev`)
- `config_source`: `${agent_home}/agent.yaml`
- `input_mode`: `subscription`, `webhook_legacy`, or `stdin_file`
- `transport_mode`: `gh_forward`, `websocket`, `webhook`, `rpc_only`, or `none`
- `runtime_dev_agent_source`: effective local agent source path when `runtime_mode=dev`
- `runtime_dev_agent_source_origin`: where that dev source came from (`flag`, `env:*`, or `default:*`)
- `subscription_reason`: why the current mode was selected (for example `empty_repos`)
- `warnings`: explicit preview/passive guardrails with expected next actions

## Event Types

The following GitHub webhook events are supported:
- `issues` - Issues opened, edited, closed, etc.
- `issue_comment` - Issue comments created, edited, deleted
- `pull_request` - PRs opened, edited, closed, etc.
- `pull_request_review` - PR reviews submitted, edited, dismissed
- `pull_request_review_comment` - PR review comments created, edited, deleted
- `check_suite` - CI check suite completed

Events are normalized to the internal `EventEnvelope` format and deduplicated using GitHub delivery IDs.

## State Persistence

Webhook mode maintains persistent state in the state directory:

### Files
- `serve-state.json` - Cursor position, dedupe index, processed event tracking
- `events.ndjson` - All received events (append-only log)
- `decisions.ndjson` - Forward/skip decisions with reasons
- `actions.ndjson` - Controller action execution results

### Deduplication
- Events are deduplicated using GitHub delivery IDs (`x-github-delivery` header)
- State persists across restarts to prevent event replay storms
- Processed event index automatically compacts to 2000 entries (newest kept)

### Cursor management
- `last_event_id` tracks the most recent event processed
- Enables safe restart without reprocessing old events
- Supports long-running controller mode

## Architecture

```
┌─────────────┐     gh webhook forward      ┌──────────────┐
│   GitHub    │ ──────────────────────────> │ holon serve  │
│  Webhooks   │                              │ (Webhook     │
└─────────────┘                              │  Server)     │
                                             └──────┬───────┘
                                                    │
                                                    ▼
                                           ┌────────────────┐
                                           │ Event          │
                                           │ Normalization  │
                                           └────────┬───────┘
                                                    │
                                                    ▼
                                           ┌────────────────┐
                                           │ Dedupe &       │
                                           │ Cursor Check   │
                                           └────────┬───────┘
                                                    │
                                                    ▼
                                           ┌────────────────┐
                                           │ Controller     │
                                           │ Agent (Docker) │
                                           └────────────────┘
```

## Troubleshooting

### Webhook server not receiving events
- Verify `gh webhook forward` is running and shows active connections
- Check firewall allows port 8080 (or your configured port)
- Ensure `--repo` matches the repository you're forwarding from
- Check `holon serve` logs for errors: `--log-level debug`

### Duplicate events
- Check that `serve-state.json` persists between runs
- Verify state directory is writable
- Ensure process has proper file permissions

### Events not reaching controller
- Check `decisions.ndjson` for skip reasons
- Verify controller skill path is correct
- Check Docker is running: `docker info`
- Review `actions.ndjson` for execution errors

### Serve starts but looks idle (passive mode)
- Check `serve-startup-diagnostics.json` and startup logs for `transport_mode=rpc_only`
- If `subscription_reason=empty_repos`, add `subscriptions.github.repos` in `agent.yaml`
- If running with `--no-subscriptions`, provide input via `--input` (or stdin with `--input -`)
- If no tick is configured, idle behavior is expected until RPC `turn/start` or injected events arrive

### Port already in use
- Choose a different port: `--webhook-port 8081`
- Stop the process using port 8080: `lsof -i :8080`

### gh-webhook extension not found
- Verify gh CLI is installed: `gh --version`
- Check extension is installed: `gh extension list`
- Install manually: `gh extension install cli/gh-webhook`
- For runtime images, the extension is auto-installed during image build

## Health Check

Webhook mode exposes a health check endpoint:

```bash
curl http://localhost:8080/health
```

Returns:
```json
{
  "status": "ok",
  "time": "2026-02-08T15:30:00Z"
}
```

## Migration

### Webhook Path Change

**Old path (deprecated)**: `/webhook`
**New path**: `/ingress/github/webhook`

The legacy `/webhook` path remains supported for backward compatibility but will log a deprecation warning. Please update your integrations to use the new provider-specific path.

#### Why this change?

This aligns with [RFC-0005](../rfc/0005-serve-api-direction.md) which establishes:
- Provider-specific ingress paths (`/ingress/<provider>/webhook`)
- Separation of ingress from control plane APIs
- Future-proof design for multi-provider support

#### Updating integrations

If you're using `gh webhook forward`, no changes are needed - just ensure you're targeting the correct port.

For custom integrations, update your webhook URLs:
- Before: `http://localhost:8080/webhook`
- After: `http://localhost:8080/ingress/github/webhook`

The old path will be removed in a future release after a deprecation period.

## Limitations

- **Local-only**: Webhook mode requires `gh webhook forward`, which only works locally (not in CI/production)
- **No auth**: The HTTP server has no authentication - only use behind a firewall or in dev environments
- **Single repo**: Each `holon serve` instance handles events for one repository
- **Port binding**: Only one webhook server per port

## Future Enhancements (Phase D1)

See GitHub issue [#573](https://github.com/holon-run/holon/issues/573) for planned enhancements:
- Hosted webhook ingress service
- `since_id` catch-up/replay protocol
- Auth model for App-installed and non-App usage
- Production deployment support

## Future: JSON-RPC Control Plane

See [RFC-0005](../rfc/0005-serve-api-direction.md) for the planned JSON-RPC control plane API:
- Status/pause/resume methods
- Log streaming
- Structured RPC protocol based on OpenAI Codex schemas

## See Also

- [GitHub Issue #573](https://github.com/holon-run/holon/issues/573) - Epic for hosted ingress + replay
- [gh webhook forward documentation](https://cli.github.com/manual/gh_webhook_forward)
- [Controller documentation](/docs/controller.md)
