---
title: Host local apps
summary: Build a small agent-owned app, submit a request, and verify its queue events in the browser.
order: 240
---

# Host local apps

Build a card that shows its owning agent, submits a `ping` request, and displays
queue events. This verifies the App SDK connection without granting the app
permission to run arbitrary work. For the underlying model, see
[Local Apps](../concepts/local-apps.md).

## Before you start

- Use an existing agent and confirm its Agent Home. Ask the agent to create the
  app there, or use its confirmed host path; a custom data directory can change
  that location. Do not overwrite an existing `ping-card` app.
- Run `holon daemon status` to check the runtime. Start it with
  `holon daemon start` if needed.
- Use the runtime's browser origin and, for remote access, an authenticated
  session. See [Connect to a remote runtime](connect-remote-runtime.md).
- Host only code you trust. Apps share the runtime's origin and session;
  they are not isolated from host credentials.

## 1. Create the directory and optional manifest

In the confirmed Agent Home, create `apps/ping-card/`. A root `index.html`
is enough to host it without a manifest: the runtime uses the directory name
for `id` and `name`, `"0.0.0"` for `version`, `"index.html"` for `entry`,
and `null` for `description`. These defaults exist only in memory; Holon
does not write a manifest or change your files.

For the display name and description used in this example, optionally save
`apps/ping-card/manifest.json`:

```json
{
  "id": "ping-card",
  "name": "Ping Card",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "Ping the agent from a static card"
}
```

## 2. Write the page

Save this as `apps/ping-card/index.html`:

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
  <pre id="events"></pre>

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
        observeEvents();
      } catch (err) {
        statusEl.textContent = `Failed to load context: ${err.message}`;
      }
    }

    pingBtn.addEventListener("click", async () => {
      pingBtn.disabled = true;
      outputEl.textContent = "Sending request...";
      try {
        const res = await window.Holon.request("ping", { timestamp: Date.now() });
        outputEl.textContent = "Queue receipt (not a completed result):\n" +
          JSON.stringify(res, null, 2);
      } catch (err) {
        outputEl.textContent = `Error: ${err.message}`;
      } finally {
        pingBtn.disabled = false;
      }
    });

    async function observeEvents() {
      const eventsEl = document.getElementById("events");
      try {
        for await (const item of window.Holon.events()) {
          const event = item.event;
          eventsEl.textContent += `${event.type}: ${event.message_id}\n`;
        }
      } catch (err) {
        eventsEl.textContent += `Stream ended: ${err.message}`;
      }
    }

    init();
  </script>
</body>
</html>
```

## 3. Open and test the app

Open `/apps/<agent-id>/ping-card/` on your runtime's browser origin, replacing
`<agent-id>` with the owning agent ID. For example, a local runtime using the
default port and the `main` agent serves the page at
`http://127.0.0.1:7878/apps/main/ping-card/`. Do not open the HTML as a `file:` URL.

1. Check that the page names the expected agent. This comes from `context()`.
2. Wait for the event connection to open, then click **Ping Agent** once.
3. Check the queue receipt for `status: "accepted"` and a `message_id`.
4. Match that ID to a `message_enqueued` event. A runnable agent may also emit
   `message_processing_started`.

The stream reports queue and processing events, not completed results. A `ping`
request is structured input for the agent, not a built-in executable command.
Use the [work observation guide](observe-agent-work.md) to check what the agent
actually did. Operator instructions must separately authorize any business
workflow that the app requests.

## If it does not work

- **App missing or rejected:** without a manifest, provide a readable root
  `index.html`; no other entry is guessed. If a manifest exists, check its
  required fields, its `id` (`ping-card` here), and its entry path. Invalid,
  oversized, unreadable, directory, dangling-link, or escaping-link manifests
  are errors, not a fallback to defaults. Keep paths non-hidden and contained
  in the app root.
- **Authentication fails:** open the app from the runtime origin after logging
  in or pairing. Never put a bearer token in the HTML or JavaScript.
- **SDK unavailable:** check the `holon.js` response. Source builds must include
  the App SDK assets; an HTML page alone cannot supply them.
- **No events:** check the browser's network panel for the `/events` stream.
  Reload and submit another request after it connects. Do not infer failure
  from an event missed during a disconnect.
- **External script or request blocked:** inspect browser CORS, the app's own
  CSP, and any operator-configured App CSP. Holon sends no platform App CSP
  by default. Do not import untrusted code into the runtime's shared session.
- **`413` / `app_asset_too_large`:** the complete file exceeds the daemon's
  `local_apps.max_asset_bytes` (64 MiB by default), even if requesting a range.
- **`503` / `app_asset_busy`:** all 16 shared asset streams are occupied.
  Honor `Retry-After: 1` and retry; SDK/API requests do not use these permits.

## Host an existing static build

For a static site or uncompressed, single-threaded Unity Web build, copy only
the intended publication output into `apps/<app-id>/` in the confirmed Agent
Home. Keep its relative layout and root `index.html`, then open
`/apps/<agent-id>/<app-id>/` after logging in. No separate HTTP server,
manifest, or SDK integration is needed just to serve static files. If the
build uses a different HTML entry name, provide an explicit manifest with
all required fields and set `entry`; Holon does not guess or rename it.

Ordinary files are served without an extension allowlist, including WASM
(`application/wasm`) and data or unknown types (`application/octet-stream`).
Hidden files and path/symlink escapes are rejected; there is no directory
listing or server-side execution. Do not copy source trees, secrets, or
private files into the app directory.

The daemon-wide size and optional CSP settings, CLI examples, and activation
semantics are in [Daemon configuration](../reference/local-apps.md#daemon-configuration).
With a running daemon, `holon config set` and `holon config unset` persist
changes through the existing configuration API and schedule reload. After a
successful reload, subsequent App requests use the new settings; in-flight
streams retain their acquired policy. Direct file edits do not automatically
reload configuration; offline settings take effect on daemon startup or restart.
These settings do not change Web GUI or Explorer sandbox policy.

Check entry, script, WASM, and data responses in the browser network panel,
then exercise the application rather than treating a successful download as
a successful game run. `.gz`, `.br`, and `.unityweb` are served as ordinary
files; runnable precompressed builds and original MIME/encoding negotiation
are not promised. Multithreaded Unity and COOP/COEP are outside this slice.
Real-game and real Mac/Safari acceptance remain outstanding; automated HTTP
checks do not establish those results. Existing services need the updated
binary and configuration activation; this guide does not restart them.

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

<a id="example-building-a-minimal-app"></a>
<a id="1-create-the-directory-and-manifest"></a>
<a id="2-write-indexhtml"></a>
<a id="3-open-in-browser"></a>
The example above replaces the earlier minimal-app walkthrough.

## Next Steps

- [Use the Web GUI](use-web-gui.md) to manage agents and inspect files.
- [HTTP control plane reference](../reference/http-control-plane.md) covers
  routes and authentication.
