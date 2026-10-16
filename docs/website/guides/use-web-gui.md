---
title: Use the Web GUI
summary: Drive agents, work items, and skills from the browser.
order: 170
---

# Use the Web GUI

Use the Web GUI to send an agent a request, follow its work, and inspect the
result and files. Start with the steps below; the page descriptions cover
settings and less frequent tasks.

## Prerequisites

- A Holon runtime with the embedded Web GUI and a working model route.
  For model setup, follow [Configure models](configure-models.md).
- A browser that can reach the runtime's base URL.
- A control token or organization login if the runtime requires authentication.
  For another host, first follow [Connect to a remote runtime](connect-remote-runtime.md).

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

## Authentication and Login

When connecting remotely or running with authentication enabled, navigate to
`/login` to authenticate:

- **Local Token mode** (`auth.mode = "local"`): Enter your static control token.
  The runtime exchanges it for an HttpOnly `holon_session` cookie so you stay
  logged in.
- **OIDC Single Sign-On** (`auth.mode = "oidc"`): Click **Continue with organization login**
  to authenticate through your company's identity provider.

For full IdP setup and session timeouts, see [Configure OIDC authentication](/guides/configure-oidc-authentication.md).

## Send a request

1. Open the Dashboard and select an existing agent, or create one from the
   template catalog.
2. Check the agent identity and workspace in the side panel before asking it
   to read or change files.
3. Enter a request in the conversation input. For a connection check, use:

   ```text
   Reply with "Web GUI connection verified". Do not change any files.
   ```

4. Wait for the new response. Expand the turn's execution details if the agent
   reports a failure or you need to inspect a tool result.

## Follow a WorkItem or Task

For work that must survive across turns, ask the agent to track the objective,
plan, and acceptance criteria. Inspect its current work in the right panel:

- Read the WorkItem's plan status and checklist to understand the goal.
- Open a Task detail to inspect a background command or child-agent result.
- If the agent is waiting, read the wait reason before sending another request.
  It may need operator input, a task result, or an external event.
- Read the completion brief and check the promised artifacts before accepting
  the result. A finished Task does not necessarily finish the WorkItem.

See [WorkItems and Tasks](../concepts/work-items-and-tasks.md) for the distinction.

## Inspect a result or file

1. Read the agent's completion brief for the outcome and verification status.
2. Follow a file reference in the response, or open **Files** in the context
   panel.
3. Check **File information** for the workspace, execution root, and path.
   Files belong to the runtime host, which may be different from your browser's
   computer.
4. Preview the content, use **Source** for literal Markdown, or download the
   full file when the preview is truncated.

The [Workspace File Browser](#workspace-file-browser) section below covers
navigation and host-side Finder integration.

## Pages

Use these screens for the task you need:

### Dashboard

The Dashboard provides a runtime overview:

- **Agent roster** — all agents with their status (Awake, Asleep, Booting),
  pending message counts, and current model.
- **Runtime health** — scheduler posture, wake hints, and recent activity.
- **Task list** — click a task to open its detail panel showing status,
  command, workdir, and output. Task events (creation, status updates,
  completion) refresh the list in real time.
- **Quick actions** — create agents, attach workspaces, and view agent detail.
- **Agent lifecycle** — start, stop, and delete agents from the dashboard.
  Before deleting, preserve needed results and review the confirmation and
  any blockers. Include private children only when you intend to remove them.

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
- **Tool execution details** — expand individual tool calls in the message
  stream to inspect request/response payloads, timing, and metadata.
  Tool result panels show structured output alongside raw data, making
  it easier to debug agent behavior.
- **Live updates** — watch new output, tool activity, and completion briefs
  as the runtime sends events.
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
[Agent Templates reference](../reference/agent-templates.md).

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

For CLI-based skill management, see [Use skills](use-skills.md).

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

The Holon macOS menu app defaults to `--desktop-integration` for its managed
daemon and preserves the opt-in independently of LAN access. At a
localhost/loopback browser entry point, the file actions menu offers
**Show in Holon host's Finder**. It locates the selected
file without opening or executing its contents. Other platforms continue to
support preview, copying, and downloading.

Direct CLI launches default to off and require an explicit option. The menu app
retains the independent setting; a CLI restart retains it unless overridden
with `--desktop-integration=false`. LAN and Serve do not disable Finder.
Authorized remote API calls can reveal a validated path on the Holon host;
the Web UI hides this action at LAN/Serve origins only to prevent misclicks.
A loopback address does not prove that files belong to the computer displaying
the browser. The
connection indicator says **Loopback address**, rather than claiming that the
runtime is local.

### Settings

Configure Holon from the browser:

- **Model settings** — view and change the default model, override per-agent
  models, and set reasoning effort. Drag fallback models to change their
  priority. See [Configure models](configure-models.md) for route setup.
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
- **Decision settings** — configure decision routes and providers directly from the browser. Switch between remote endpoints (such as TypeSafe Jev or OpenAI-compatible models) and zero-egress local ONNX presets, view decision telemetry, and toggle advisory tool access.
- **Device pairing** — generate short-lived, single-use pairing QR codes and links directly from the Settings page. Scan with the Holon Android client or open in another browser to establish an authenticated session without exposing your long-lived control token.
- **Tailscale Serve** — inspect status and toggle Tailscale Serve from the browser. When enabled, your Holon instance is securely shared across your private tailnet with automatic HTTPS certificates, requiring no manual firewall or reverse proxy setup.

Pairing prefers a valid Serve HTTPS destination; the phone must join the tailnet
and have ACL access. A custom destination overrides the default, and LAN remains
an alternative. From an HTTPS reverse proxy, the current origin can be used
without passing `--advertise`; serve Web and API at the same origin's root.
OIDC users use normal login rather than ticket pairing. Serve needs effective
control authentication and a loopback-capable backend, but does not require LAN
access. The menu may prepare a private token and restart its managed daemon after
an explicit enable confirmation; the Web UI only provides configuration guidance.
Enabling menu LAN access binds all IPv4 interfaces (potentially public ones), not
just one LAN adapter, while retaining loopback for Serve.

### Internationalization (i18n)

Choose English or Simplified Chinese in [Settings](#settings). Changing the
interface language does not translate agent responses or file contents.

The Settings page also includes a **search provider** section for configuring
web search and native search providers through the browser.

### UI Icons

Use icon tooltips to identify an action before selecting it. For file actions,
see [Workspace File Browser](#workspace-file-browser).

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

Expand the right panel when you need more room to read a detail view.

## Remote Access

The Web GUI works with Holon's remote access modes. When the daemon is
configured for remote access (tunnel, tailnet, or LAN), open the GUI URL
through the same endpoint:

```text
# Example: LAN access from another machine on the same network
http://<daemon-host>:7878/
```

Configure CORS if accessing from a different origin. See
[Remote Access](/guides/connect-remote-runtime) and
[Configuration](/reference/configuration) for details.

## Embedded vs Development Build

Release binaries include the Web GUI. Start the daemon and open its browser
origin; normal use needs no Node.js installation or UI build.

For UI development, follow [Run Holon locally](../maintainers/local-runtime.md#web-gui-development).

## Performance Diagnostics

If a request is slow, first inspect the agent's wait reason and execution
details. For counters across the runtime, use the
[observability guide](configure-observability.md). The endpoint and field
definitions are in [Runtime performance snapshots](../reference/observability.md#runtime-performance-snapshots).

### Job Monitoring

Long-running operations such as skill installation run as tracked jobs
instead of blocking the request:

- **Inline progress** — the Skills page shows a progress indicator for the
  running job, with success or error feedback when it finishes.
- **Job API** — the runtime exposes the job read model at
  `/api/jobs/{job_id}` for status, phase, and progress items.

## Verify your session

Check that your test request appears under the intended agent and has a new
response. For file work, open the promised artifact and check its execution
root. Refresh the page and confirm that the persisted conversation and work
status remain visible; live animation alone is not evidence of a saved result.

## Troubleshooting

- **Login loops:** Check the selected auth mode and the browser's session.
  For OIDC, verify the callback URL using the
  [OIDC guide](configure-oidc-authentication.md).
- **Page loads but requests fail:** Check the API origin, proxy routing, and
  authentication. Follow [Remote access](connect-remote-runtime.md).
- **No new activity:** Read the agent's wait reason and Task status. Refresh
  once to distinguish a disconnected event stream from paused work; avoid
  resubmitting the same request until you know whether it was accepted.
- **Skill installation still running:** Inspect its job status before retrying.
  The initial request can return before installation finishes.
- **File link fails:** Check the registered workspace and execution root.
  A moved or removed worktree may make an old reference unavailable.
- **Model unavailable:** Check provider credentials and model routing in
  [Configure models](configure-models.md).

## See Also

- [Run your first agent](../getting-started/first-agent.md) — the first-session walkthrough
- [Connect to a remote Holon runtime](/guides/connect-remote-runtime) — use the GUI from another machine
- [Troubleshoot a Holon task](/guides/troubleshooting) — diagnose common problems
- [Configuration Reference](/reference/configuration) — CORS, ports, and
  control plane settings
- [Configure OIDC authentication](/guides/configure-oidc-authentication.md) — set up team SSO and session policies
