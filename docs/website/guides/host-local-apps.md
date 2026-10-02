---
title: Host local apps
summary: Host agent-owned HTML/JS static applications, interact with agents through the App SDK, and serve lightweight UI tools.
order: 25
---

# Host Local Apps

Starting in v0.47.0, Holon includes a Local App Engine. Agents can host lightweight, self-contained HTML and JavaScript applications stored directly inside their `agent_home/apps/` directory.

Clients access these applications through logical URLs rather than physical filesystem paths, protecting host directory structures.

## How Local Apps Work

Each app lives in its own subdirectory under the agent's home:

```text
~/.holon/agents/<agent_id>/apps/<app_id>/
├── manifest.json
├── index.html
├── style.css
└── app.js
```

When a user or browser visits `/apps/<agent_id>/<app_id>/`, Holon:

1. Validates the directory and `manifest.json`.
2. Applies baseline Content Security Policy (CSP) headers and traversal checks.
3. Serves the configured entry document and requested static assets.
4. Mounts the built browser SDK at `/apps/<agent_id>/<app_id>/holon.js`.

## Manifest Contract

Every app directory must contain a valid `manifest.json` (maximum 64 KiB):

```json
{
  "id": "status-monitor",
  "name": "Status Monitor",
  "version": "1.0.0",
  "entry": "index.html",
  "description": "Displays agent status and triggers health checks"
}
```

### Manifest Fields

| Field | Type | Description |
|---|---|---|
| `id` | string | App identifier. Must match the directory name exactly. |
| `name` | string | Human-readable application name. |
| `version` | string | Application version string (e.g. `1.0.0`). |
| `entry` | string | Safe relative path to the entry HTML document within the app root. |
| `description` | string | Optional short summary of what the app does. |

## The Browser App SDK

Holon serves `@holon/app-sdk` directly at `holon.js` inside every app route. Include it in your entry HTML:

```html
<script src="holon.js"></script>
```

The script attaches `window.Holon` to the global browser environment with three primary methods:

### 1. `window.Holon.context()`

Fetches execution metadata and session state for the running app:

```javascript
const ctx = await window.Holon.context();
console.log(ctx.agent_id, ctx.app_id, ctx.session.authenticated);
```

Returns:

```json
{
  "sdk_version": "1",
  "agent_id": "main",
  "app_id": "status-monitor",
  "session": { "authenticated": true }
}
```

### 2. `window.Holon.request(requestType, payload, requestId)`

Dispatches a structured request to the agent queue:

```javascript
const response = await window.Holon.request("run_diagnostics", { verbose: true });
console.log("Enqueued request ID:", response.request_id);
```

The call sends a `POST` to `/apps/<agent_id>/<app_id>/request`, recording origin provenance.

### 3. `window.Holon.events(options)`

Streams app lifecycle events over Server-Sent Events (SSE). It returns an `AsyncIterable<AppEvent>` and accepts an options object with an optional `AbortSignal`:

```javascript
const controller = new AbortController();

try {
  for await (const event of window.Holon.events({ signal: controller.signal })) {
    console.log("Received event:", event);
  }
} catch (err) {
  if (err.name !== "AbortError") {
    console.error("Stream error:", err);
  }
}

// Breaking out of the loop or calling controller.abort() closes the SSE stream.
```

## Security Invariants

The Local App Engine enforces several strict security boundaries:

- **Same-Origin Session:** Hosted apps share the origin with the Holon daemon. They inherit ambient session cookie authentication for control-plane calls, but do not gain additional privileges.
- **Content Security Policy (CSP):** The server sends a baseline CSP restricting network connections and external resource loading to the same origin (`'self'`), with `data:` URIs allowed for images and fonts. Same-origin scripts and inline scripts/styles (`'unsafe-inline'`) are allowed so self-contained UI components work without external bundlers. Cross-origin scripts, embeds, and outside network calls are blocked. Stronger isolation (such as unique origins or sandboxed iframes) is out of scope for this slice.
- **Asset Boundaries:** Static assets are capped at 8 MiB per file. Holon only serves allowlisted file types (HTML, JS, CSS, JSON, TXT, SVG, common images, and common fonts). Executable binaries or unknown extensions return an error.
- **Path Confinement:** Every request canonicalizes the target path. Requests attempting path traversal (`../`) or traversing symlinks outside the app directory are rejected immediately.

## Example: Building a Minimal App

Here is a minimal app that inspects agent context and sends a prompt.

### 1. Create the Directory and Manifest

Create directory `~/.holon/agents/main/apps/ping-card/` and save `manifest.json`:

```json
{
  "id": "ping-card",
  "name": "Ping Card",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "Ping the agent from a static card"
}
```

### 2. Write `index.html`

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Agent Ping Card</title>
  <style>
    body { font-family: sans-serif; padding: 2rem; max-width: 480px; margin: auto; }
    button { padding: 0.5rem 1rem; cursor: pointer; }
    pre { background: #f4f4f4; padding: 1rem; border-radius: 4px; overflow-x: auto; }
  </style>
</head>
<body>
  <h2>Agent Ping Card</h2>
  <p id="status">Loading context...</p>
  <button id="ping-btn" disabled>Ping Agent</button>
  <pre id="output"></pre>

  <script src="holon.js"></script>
  <script>
    const statusEl = document.getElementById("status");
    const pingBtn = document.getElementById("ping-btn");
    const outputEl = document.getElementById("output");

    async function init() {
      try {
        const ctx = await window.Holon.context();
        statusEl.textContent = `Connected to agent: ${ctx.agent_id}`;
        pingBtn.disabled = false;
      } catch (err) {
        statusEl.textContent = `Failed to load context: ${err.message}`;
      }
    }

    pingBtn.addEventListener("click", async () => {
      pingBtn.disabled = true;
      outputEl.textContent = "Sending request...";
      try {
        const res = await window.Holon.request("ping", { timestamp: Date.now() });
        outputEl.textContent = JSON.stringify(res, null, 2);
      } catch (err) {
        outputEl.textContent = `Error: ${err.message}`;
      } finally {
        pingBtn.disabled = false;
      }
    });

    init();
  </script>
</body>
</html>
```

### 3. Open in Browser

Ensure the daemon is running:

```bash
holon daemon start
```

Visit `http://127.0.0.1:7878/apps/main/ping-card/` in your browser. The page will load the card, query `context()`, and allow sending structured ping requests to the `main` agent.

## Next Steps

- [Use the Web GUI](/guides/use-web-gui.md) — Manage agents and browse workspace files.
- [HTTP control plane reference](/reference/http-control-plane.md) — Inspect `/apps` routes and authentication schemas.
- [Automate over HTTP](/guides/automate-over-http.md) — Programmatic workflows using HTTP endpoints.
