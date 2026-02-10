# PM/Dev Dual-Role Demo Runbook

This runbook demonstrates two autonomous `holon serve` instances using one shared runtime skill and different role identities:

- PM instance: planning, issue creation/assignment, PR review/merge decisions
- Dev instance: assignment-driven implementation and PR updates

## Prerequisites

- Docker is available locally.
- GitHub webhooks can be forwarded to localhost (`gh webhook forward`).
- Two GitHub tokens with distinct identities:
  - PM identity token
  - Dev identity token

## Directory Layout

Use isolated state/workspace per role to keep memory and channel files separate:

```bash
mkdir -p .holon/pm-state .holon/dev-state .holon/pm-workspace .holon/dev-workspace
```

## Start PM Instance

```bash
HOLON_GITHUB_TOKEN="$PM_GITHUB_TOKEN" \
holon serve \
  --repo owner/repo \
  --webhook-port 8787 \
  --state-dir .holon/pm-state \
  --controller-workspace .holon/pm-workspace \
  --controller-skill skills/github-controller \
  --controller-role pm \
  --tick-interval 5m
```

## Start Dev Instance

```bash
HOLON_GITHUB_TOKEN="$DEV_GITHUB_TOKEN" \
holon serve \
  --repo owner/repo \
  --webhook-port 8788 \
  --state-dir .holon/dev-state \
  --controller-workspace .holon/dev-workspace \
  --controller-skill skills/github-controller \
  --controller-role dev
```

## Forward GitHub Webhooks To Both Instances

If your local setup can only forward to one endpoint, run two forwarding processes with different filters or use a local fan-out proxy.

Example direct forwarding targets:

- PM: `http://127.0.0.1:8787/ingress/github/webhook`
- Dev: `http://127.0.0.1:8788/ingress/github/webhook`

## Expected Artifacts

PM state directory:

- `.holon/pm-state/events.ndjson`
- `.holon/pm-state/decisions.ndjson`
- `.holon/pm-state/actions.ndjson`
- `.holon/pm-state/controller-state/event-channel.ndjson`
- `.holon/pm-state/controller-state/goal-state.json`

Dev state directory:

- `.holon/dev-state/events.ndjson`
- `.holon/dev-state/decisions.ndjson`
- `.holon/dev-state/actions.ndjson`
- `.holon/dev-state/controller-state/event-channel.ndjson`

## Demo Success Criteria

1. PM creates and sequences at least one implementation issue.
2. PM assigns issue to Dev identity.
3. Dev opens or updates a PR for the assigned work.
4. PM reviews and makes merge/next-step decision without manual step-by-step prompting.
