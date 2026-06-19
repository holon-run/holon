---
title: Web GUI
summary: Use Holon's embedded web interface to manage agents, monitor runtime state, and configure settings from a browser.
order: 25
---

# Web GUI

Holon includes an embedded web GUI served directly from the daemon. No separate
frontend build or deployment is needed — start the daemon and open a browser.

## Quick Start

```bash
# Start the daemon (Web GUI is enabled by default)
holon daemon start
```

Then open [http://127.0.0.1:7878/app/](http://127.0.0.1:7878/app/) in a
browser.

The Web GUI is served from embedded assets compiled into the `holon` binary.
The default CORS configuration permits `localhost` origins so the GUI works
out of the box on the same machine.

> **Note:** If you configured a custom listen address or port, adjust the URL
> accordingly.

## Pages

### Dashboard

The Dashboard provides a runtime overview:

- **Agent roster** — all agents with their status (Awake, Asleep, Booting),
  pending message counts, and current model.
- **Runtime health** — scheduler posture, wake hints, and recent activity.
- **Quick actions** — create agents, attach workspaces, and view agent detail.

### Agent Conversation

Select an agent from the Dashboard to open its conversation page:

- **Message stream** — recent messages, tool calls, and briefs displayed as a
  threaded conversation.
- **Display levels** — switch between Info (compact user-facing output),
  Verbose (tool calls and intermediate results), and Debug (full runtime
  metadata including tool execution records).
- **Input bar** — send operator messages to the selected agent.
- **Event timeline** — a sidebar timeline of recent events including turns,
  tool executions, and state transitions.

### Search

Search across agent memory from the browser:

- **Full-text search** — query runtime memory index across agents.
- **Filter by agent** — scope results to one or more agent IDs.
- **Result navigation** — click a result to view the full memory record.

### Settings

Configure Holon from the browser:

- **Model settings** — view and change the default model, override per-agent
  models, and set reasoning effort.
- **API keys** — add or update provider credentials (API keys) through the
  credential store without editing JSON files.
- **Runtime configuration** — view the current execution environment,
  attached workspaces, and policy snapshot.

### Inspector (Right Panel)

When viewing an agent or the Dashboard, the right panel shows:

- **Agent identity** — agent ID, visibility, ownership, and profile preset.
- **Current work** — active work item, plan status, and todo list.
- **Token usage** — cumulative and per-turn token consumption.
- **Active children** — spawned child agents and their status.
- **Tool latency** — per-tool call count and total duration.

## Remote Access

The Web GUI works with Holon's remote access modes. When the daemon is
configured for remote access (tunnel, tailnet, or LAN), open the GUI URL
through the same endpoint:

```bash
# Example: LAN access from another machine on the same network
http://<daemon-host>:7878/app/
```

Configure CORS if accessing from a different origin. See
[Remote Access](/guides/remote-access) and
[Configuration](/reference/configuration) for details.

## Embedded vs Development Build

| Mode | How to access | When to use |
|------|--------------|-------------|
| **Embedded** (default) | `holon daemon start` → `/app/` | Normal use |
| **Dev server** | `cd web-gui/app && npm run dev` | UI development |

The embedded build is compiled into the `holon` binary at release time via
`rust-embed`. No separate `npm` install or build step is required for
production use.

To run the development server for UI work:

```bash
cd web-gui/app
npm install
npm run dev
```

The dev server includes hot reload and uses fixture data when no Holon server
is running. Set `VITE_HOLON_API_BASE=/holon-api` to proxy through the Vite dev
server to a running Holon daemon.

## Performance Diagnostics

The Web GUI exposes runtime performance metrics at
`/control/runtime/performance`. This endpoint returns granular timing data
grouped by phase:

| Group | Metrics |
|-------|---------|
| `turn.*` | Total turn time, context build, provider round, tool execution, cleanup |
| `provider.*` | Request build, round total, retry latency |
| `tool.execution` | Cumulative tool execution timing |
| `storage.*` | Event append and state persistence timing |
| `projection.*` | Agent state projection substeps (tasks, timers, work items, etc.) |
| `http.*` | Per-route HTTP response timing |
| `scheduler.*` | Poll latency by outcome |

Each metric includes `count`, `total_ms`, `max_ms`, and `avg_ms`. Use this to
diagnose slow turns, identify expensive tools, or track provider latency over
time.

## See Also

- [Quick Examples](/guides/quick-examples) — Try Holon in a few commands
- [Remote Access](/guides/remote-access) — Connect to a remote daemon
- [Troubleshooting](/guides/troubleshooting) — Diagnose common issues
- [Configuration Reference](/reference/configuration) — CORS, ports, and
  control plane settings
