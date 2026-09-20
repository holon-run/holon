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

Then open [http://127.0.0.1:7878/](http://127.0.0.1:7878/) in a
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
- **Task list** — click a task to open its detail panel showing status,
  command, workdir, and output. Task events (creation, status updates,
  completion) refresh the list in real time.
- **Quick actions** — create agents, attach workspaces, and view agent detail.
- **Agent lifecycle** — start, stop, and delete agents directly from the
  dashboard. Deleting an agent permanently removes it and its data, with an
  option to also remove its private child agents.

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
- **Virtual scrolling** — message lists use virtualized rendering for
  smooth performance across long conversations without DOM overhead.
- **Tool execution details** — expand individual tool calls in the message
  stream to inspect request/response payloads, timing, and metadata.
  Tool result panels show structured output alongside raw data, making
  it easier to debug agent behavior.
- **Real-time SSE streaming** — connects to `/api/agents/:id/events/stream` for live token streaming, streaming reasoning thoughts, active tool execution indicators, and immediate brief updates.
- **Waiting & scheduler indicators** — clearly displays explicit wait states when agents invoke `WaitFor` (waiting on background tasks, external callbacks, or operator input), including delivery modes (`final` vs `silent`).

### Search

Search across agent memory from the browser.

Results include:

- **Excerpts** — Each result shows a contextual snippet highlighting the
  matched terms so you can evaluate relevance without opening the full
  record.
- **Expandable sources** — Click a result to expand its full content
  inline without navigating away from the search page.
- **Agent filtering** — Scope results to one or more agent IDs.
- **Full-text search** — Query the runtime memory index across agents.

### Agent Templates

Browse, install, and create agents from templates directly in the Web GUI.
Available at `/templates`:

- **Template catalog** — Browse installed templates with display name,
  description, and source information (local, remote URL, or synced source).
- **Create Agent** — Click a template to open the Create Agent dialog
  pre-filled with the template selector. The agent is initialized with the
  template's role contract and pre-installed skills.
- **Remote sources** — View and manage configured remote template sources
  (GitHub repositories). The daemon syncs templates from these sources at
  startup.
- **Template detail** — Click a template to view its full metadata including
  the template manifest, pre-installed skills, and source provenance.

For CLI-based template management, see
[Agent Templates Guide](/guides/agent-templates.md).

### Skill Management

Manage the Skill Library and agent skills from the browser:

- **Library catalog** — Browse all skills registered in the local Skill
  Library with name, description, and source information.
- **Add skill** — Import a skill from a local path, remote URL, or
  GitHub `uses` shorthand.
- **Remove skill** — Remove a skill from the library.
- **Enable/disable** — Enable or disable individual skills per agent
  with a toggle. View which skills are active for each agent.
- **Skill detail** — Click a skill to view its full metadata including
  scope, source root, and discovery path.

The Skill Management page is available at `/skills` in the Web GUI.
It is also accessible from the navigation sidebar when the daemon is
running with the embedded GUI.

Skill installation through the Web GUI is **non-blocking** — when you
add a skill via the browser, the install runs as a background job (see
[Job API](#job-monitoring)). A progress indicator shows the current
status, and the job state persists in localStorage so you can track it
across page reloads.

The Skills page also supports **updating** installed skills. A skill that
has a newer version published in its remote source shows an update button.
Clicking it queues a catalog update job that fetches the latest version.
Success and error feedback appears inline after the job completes, and
update jobs run as background tasks (see [Job API](#job-monitoring)).
Skill catalog updates also run as jobs, with progress tracked in the job
list.

For CLI-based skill management, see [Skills Guide](/guides/skills.md).

### Workspace File Browser

Open **Files** in the context side panel, or follow a file reference in a
conversation. Selecting a file opens its preview. Markdown supports **Preview**
and **Source** modes; text files support syntax highlighting.

- **Browse folder** switches back to the directory while retaining the selected
  file and reading position. Expanded panels can show directory and preview
  together.
- The location row contains the workspace selector and folder breadcrumb. Long
  paths collapse their ancestors into a menu. **File information** shows the full
  path, execution root, type, size, and modification time.
- **File actions** contains copy path, copy Markdown reference, copy web link,
  download, and refresh. **Open in new tab** provides a dedicated viewer.
- The panel's Back button returns through file navigation to its source. The
  top-right close button closes the side panel.

Files are resolved against registered workspaces and execution roots. Path
traversal and symlink escapes are blocked. Text previews are capped at 1 MB;
downloads can retrieve the complete file.

#### Optional Finder integration (macOS)

When Holon runs directly on the same Mac as your browser, explicitly enable
desktop integration at startup:

```bash
holon daemon start --access local --desktop-integration
# To enable it on an already running local instance, restart intentionally:
holon daemon restart --access local --desktop-integration
```

The Holon macOS menu app adds `--desktop-integration` automatically when it
starts or restarts its managed daemon. The file actions menu then offers **Show in Finder**. It locates the selected
file without opening or executing its contents. Other platforms continue to
support preview, copying, and downloading.

Direct CLI launches default to off and require an explicit option. The menu app
enables it on each start and restart; a CLI restart retains the setting unless
overridden with `--desktop-integration=false`. The runtime requires a
loopback-only listener. Do not enable
it for a forwarded port, reverse proxy, or container: a loopback address does
not prove that files belong to the computer displaying the browser. The
connection indicator says **Loopback address**, rather than claiming that the
runtime is local.

### Settings

Configure Holon from the browser:

- **Model settings** — view and change the default model, override per-agent
  models, and set reasoning effort. The fallback model list uses a chip
  component with drag-to-reorder support for easy priority adjustment.
- **API keys** — add or update provider credentials (API keys) through the
  credential store without editing JSON files. The settings page
  automatically determines the right credential method for each provider
  (API key input for api_key providers, device login link for OAuth
  providers like Codex).
- **Ollama discovery** — a locally running Ollama server is discovered
  automatically with no API key; its models appear in the model selectors.
- **Language** — switch the Web GUI display language. Supported languages are
  English (EN) and Simplified Chinese (ZH-CN). All UI text — navigation,
  buttons, labels, and status messages — updates in real time without a
  page reload.
- **Image generation** — configure the default image-generation model
  (`image_generation.default`) directly from the Settings page. The
  model selector shows only models that support image generation
  capability.
- **Control plane & bearer token** — configure and store Bearer authentication tokens directly in the UI when connecting to remote or authenticated Holon instances.
- **Runtime configuration** — view the current execution environment,
  attached workspaces, and policy snapshot.

### Internationalization (i18n)

The Web GUI uses react-i18next for full internationalization. A language
selector in the Settings page lets you switch between English and Simplified
Chinese. All interface text — including navigation items, button labels, form
hints, status messages, and error pages — translates instantly on selection.
Translation resources are compiled into the embedded build alongside the UI
assets, so no external files or network access are required.

The Settings page also includes a **search provider** section for configuring
web search and native search providers through the browser.

### UI Icons

Status indicators, navigation icons, and file browser icons now use lucide
icons (via lucide-react) throughout the Web GUI, replacing the previous
Unicode symbol approach. Tooltips have been streamlined for clarity, and
panel titles use a consistent `label(N)` format.

### Inspector (Right Panel)

When viewing an agent or the Dashboard, the right panel shows:

- **Agent identity** — agent ID, visibility, ownership, and profile preset.
- **Current work** — active work item, plan status, and todo list.
- **Token usage** — cumulative and per-turn token consumption.
- **Active children** — spawned child agents and their status.
- **Tool latency** — per-tool call count and total duration.

The right panel also hosts contextual detail views. For example, clicking
a task in the Dashboard opens a **Task Detail** panel showing status,
kind, command, workdir, and output right alongside the main view.

The right panel supports an expanded fullscreen mode, and the model menu
renders through a fixed-position portal so it is never clipped by layout
overflow.

## Remote Access

The Web GUI works with Holon's remote access modes. When the daemon is
configured for remote access (tunnel, tailnet, or LAN), open the GUI URL
through the same endpoint:

```bash
# Example: LAN access from another machine on the same network
http://<daemon-host>:7878/
```

Configure CORS if accessing from a different origin. See
[Remote Access](/guides/remote-access) and
[Configuration](/reference/configuration) for details.

## Embedded vs Development Build

| Mode | How to access | When to use |
|------|--------------|-------------|
| **Embedded** (default) | `holon daemon start` → `/` | Normal use |
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
is running. Set `HOLON_API_PROXY_TARGET` to point the dev server's `/api`
proxy at a running Holon daemon (it defaults to `http://127.0.0.1:7878`).

## Performance Diagnostics

The Web GUI exposes runtime performance metrics at
`/api/control/runtime/performance`. This endpoint returns granular timing data
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

### Job Monitoring

Long-running operations such as skill installation run as tracked jobs
instead of blocking the request:

- **Inline progress** — the Skills page shows a progress indicator for the
  running job, with success or error feedback when it finishes.
- **Job API** — the runtime exposes the job read model at
  `/api/jobs/{job_id}` for status, phase, and progress items.

## See Also

- [Quick Examples](/guides/quick-examples) — Try Holon in a few commands
- [Remote Access](/guides/remote-access) — Connect to a remote daemon
- [Troubleshooting](/guides/troubleshooting) — Diagnose common issues
- [Configuration Reference](/reference/configuration) — CORS, ports, and
  control plane settings
