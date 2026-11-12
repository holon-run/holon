---
title: Build and host local interactive apps
summary: Ask your agent in conversation to create a web dashboard under its apps directory, submit structured requests with window.Holon.request(), and inspect live queue events without running CLI commands.
order: 240
---

# Build and host local interactive apps

A Local App is a lightweight, agent-owned web interface served directly from the agent's `apps/` directory. Instead of scrolling through long chat transcripts to manage review queues, inspection logs, or structured approvals, you can have your agent host a dedicated interactive dashboard.

This guide shows how to ask an agent in conversation to create a Task Board app, connect to the agent runtime via the App SDK (`holon.js`), submit structured action requests, and stream live queue events in the browser. For the underlying mental model and security rules, see [Local Apps](../concepts/local-apps.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`) and a [configured model](./configure-models.md).
- An existing agent (such as `main` or a dedicated assistant created from a template).
- No need to locate host directories on disk or run CLI commands: asking the agent in conversation to create files under its own `apps/` directory automatically places them in the agent's home storage.

## Steps

### 1. Ask the agent in conversation to build the app

In the Web GUI chat, ask your agent to create a task review board under its `apps/` directory:

> "Please build a Local App named `task-board` in your `apps/task-board/` directory. Include a `manifest.json` and an `index.html`. The page should load `holon.js`, display your agent ID from `window.Holon.context()`, let me submit an approval action via `window.Holon.request()`, and stream live queue events from `window.Holon.events()`."

The agent writes the files into its durable storage.

The optional `manifest.json` provides metadata and display information:

```json
{
  "id": "task-board",
  "name": "Task Review Board",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "Interactive review dashboard hosted by the agent"
}
```

The `index.html` page uses the bundled `holon.js` client facade:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Agent Task Board</title>
  <style>
    body { font-family: system-ui, -apple-system, sans-serif; padding: 2rem; max-width: 600px; margin: auto; line-height: 1.5; }
    .card { border: 1px solid #e2e8f0; border-radius: 8px; padding: 1.5rem; margin-bottom: 1rem; background: #fff; }
    .badge { display: inline-block; padding: 0.2rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: 600; background: #edf2f7; color: #4a5568; }
    button { background: #2563eb; color: #fff; border: none; padding: 0.6rem 1.2rem; border-radius: 6px; cursor: pointer; font-weight: 500; }
    button:disabled { background: #94a3b8; cursor: not-allowed; }
    pre { background: #0f172a; color: #e2e8f0; padding: 1rem; border-radius: 6px; overflow-x: auto; font-size: 0.85rem; }
  </style>
</head>
<body>
  <div class="card">
    <h2>Agent Task Board</h2>
    <p id="context-info">Connecting to agent runtime...</p>
    <div style="margin: 1rem 0;">
      <span class="badge" id="agent-badge">Agent: Loading...</span>
    </div>
    <button id="action-btn" disabled>Approve Item #101</button>
  </div>

  <div class="card">
    <h3>Latest Request Receipt</h3>
    <pre id="receipt-output">No request submitted yet.</pre>
  </div>

  <div class="card">
    <h3>Live Agent Event Stream</h3>
    <pre id="events-log">Listening for runtime events...</pre>
  </div>

  <script src="holon.js"></script>
  <script>
    const contextEl = document.getElementById("context-info");
    const agentBadge = document.getElementById("agent-badge");
    const actionBtn = document.getElementById("action-btn");
    const receiptOutput = document.getElementById("receipt-output");
    const eventsLog = document.getElementById("events-log");

    async function init() {
      try {
        const ctx = await window.Holon.context();
        contextEl.textContent = `Connected to session (${ctx.surface || "web-gui"}).`;
        agentBadge.textContent = `Agent: ${ctx.agent_id}`;
        actionBtn.disabled = false;
        observeEvents();
      } catch (err) {
        contextEl.textContent = `Failed to load context: ${err.message}`;
      }
    }

    actionBtn.addEventListener("click", async () => {
      actionBtn.disabled = true;
      receiptOutput.textContent = "Submitting request to agent queue...";
      try {
        const res = await window.Holon.request("review_item", {
          item_id: "item-101",
          decision: "approve",
          timestamp: Date.now()
        });
        receiptOutput.textContent = "Queue receipt (request accepted into queue):\n" +
          JSON.stringify(res, null, 2);
      } catch (err) {
        receiptOutput.textContent = `Error: ${err.message}`;
      } finally {
        actionBtn.disabled = false;
      }
    });

    async function observeEvents() {
      try {
        for await (const item of window.Holon.events()) {
          const event = item.event;
          const time = new Date().toLocaleTimeString();
          eventsLog.textContent = `[${time}] ${event.type}: ${event.message_id || ""}\n` + eventsLog.textContent;
        }
      } catch (err) {
        eventsLog.textContent = `Event stream disconnected: ${err.message}\n` + eventsLog.textContent;
      }
    }

    init();
  </script>
</body>
</html>
```

### 2. Open and test the app in your browser

Open the logical route `/apps/<agent-id>/task-board/` in your browser. For example, if your Web GUI runs at `http://localhost:8080` and your agent is `main`:

`http://localhost:8080/apps/main/task-board/`

*(Do not open the HTML file directly as a `file://` URL; the App SDK requires the runtime HTTP origin).*

1. Check that the page names the expected agent. This comes from `context()`.
2. Wait for the event connection to open, then click **Approve Item #101**.
3. Check the receipt output: it returns `{ "status": "accepted", "message_id": "..." }`.
4. Observe the live events log: as the runtime processes the request, you will see `message_enqueued` followed by `message_processing_started`.

### 3. Explicit trust classification and security boundaries

Local Apps operate under strict runtime security boundaries:
- **Queue receipt vs. completed work**: Calling `window.Holon.request()` places the request into the agent's inbox queue; it returns an acceptance receipt, not the final execution output. The agent decides how and when to execute the work according to its schedule and rules.
- **Untrusted input classification**: App requests are classified with `origin: app_ingress` and untrusted provenance. An app button cannot impersonate the operator or bypass the agent's `AGENTS.md` boundaries.
- **Same-origin session protection**: Local Apps share the runtime origin and authenticated session. Host only code authored by your trusted agents. For stricter network isolation, you can configure daemon-wide App Content Security Policy (CSP).

## When to build a Local App vs. a Markdown report

- **Use a Markdown report** when the agent's output is informational, linear, or periodic (e.g. daily standup summaries, documentation drift audits, or code review findings).
- **Use a Local App** when the workflow requires ongoing interactive filtering, visual status dashboards, or repeated structured actions (e.g. approving items one-by-one from a triage queue).

## Host an existing static build

You can also ask an agent to host pre-built static documentation, static web assets, or single-threaded Unity Web builds:
- Place the exported files into `apps/<app-id>/` under the agent's home.
- Ensure `index.html` exists at the root.
- Static assets (including `.wasm`, `.data`, images, and stylesheets) are served directly without server-side execution.
- Large files up to 64 MiB (the default `local_apps.max_asset_bytes`) are supported.

## If it does not work

- **App missing or 404**: Check that the directory name matches the URL route (`/apps/<agent-id>/<app-id>/`). If a `manifest.json` exists, confirm its `id` matches `<app-id>` and its `entry` file exists.
- **Authentication failed (401)**: Access the app from the same browser where you are logged into the Web GUI. Never put bearer tokens or secrets inside the app's HTML.
- **App SDK `holon.js` not found**: The App SDK is injected automatically by the Holon runtime on the `/apps/{agent}/{app}/holon.js` path. Ensure you access the app through the Holon HTTP server rather than an external web server.
- **`503 app_asset_busy`**: All shared asset read streams are temporarily occupied. The browser will automatically retry with `Retry-After: 1`.

## Contract details

<a id="how-local-apps-work"></a>
For ownership and request flow, read [Local Apps](../concepts/local-apps.md).

<a id="manifest-contract"></a>
<a id="manifest-fields"></a>
The [manifest contract](../reference/local-apps.md#manifest-contract) defines
required fields and validation.

<a id="the-browser-app-sdk"></a>
<a id="1-windowholoncontext"></a>
<a id="2-windowholonrequestrequesttype-payload-requestid"></a>
<a id="3-windowholoneventsoptions"></a>
The [Browser SDK reference](../reference/local-apps.md#browser-sdk) covers
return values and stream limits.

<a id="security-invariants"></a>
See [hosting and access limits](../reference/local-apps.md#hosting-and-access-limits)
for asset restrictions, authentication, and CSP.

## Next Steps

- Read [Local Apps concept](../concepts/local-apps.md) for detailed architecture and life-cycle semantics.
- Read [Local Apps reference](../reference/local-apps.md) for full manifest specifications and daemon configuration options.
- Read [Observe agent work](observe-agent-work.md) to inspect how the agent executes tasks queued from your app.
- Read [HTTP control plane reference](../reference/http-control-plane.md) for
  routes and authentication.
