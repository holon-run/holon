---
title: Local App reference
summary: Manifest fields, browser SDK methods, event scope, and hosting limits for agent-owned apps.
order: 85
---

<a id="local-app-reference"></a>

# Local App reference

A Local App is a static application stored in an agent's `apps/<app-id>/`
directory and served at `/apps/<agent-id>/<app-id>/`. See
[Local Apps](../concepts/local-apps.md) for the model and
[Host local apps](../guides/host-local-apps.md) for a working example.

<a id="manifest-contract"></a>

## Manifest contract

The root `manifest.json` is optional. Only when that directory entry is
absent, the runtime derives the following metadata in memory:

| Field | Default without a manifest |
|---|---|
| `id`, `name` | App directory name |
| `version` | `"0.0.0"` (undeclared-version placeholder, not a cache fingerprint) |
| `entry` | `"index.html"` |
| `description` | `null` |

The default entry must be a readable regular file inside the app root.
The runtime does not guess another HTML file, recurse for an entry, create
directories, or write a manifest. A missing `manifest.json` asset request
still returns `404`; discovery exposes the derived metadata, not a virtual
downloadable manifest.

If present, `manifest.json` must be no larger than 64 KiB and retain these
required fields:

| Field | Required | Contract |
|---|---|---|
| `id` | Yes | Must match the app directory name and requested app ID. |
| `name` | Yes | Nonempty display name. |
| `version` | Yes | Nonempty version string. |
| `entry` | Yes | Safe, nonempty relative path to an HTML file inside the app root. |
| `description` | No | Short description. |

Invalid JSON or fields, an unreadable manifest, a directory, a dangling or
escaping symlink, or a manifest exceeding 64 KiB is an error even if
`index.html` exists. A contained symlink must resolve to a readable regular
file within the same size bound. No malformed explicit manifest falls back
to defaults. Custom entries remain supported; a missing explicit entry file
continues to return `404` when accessed.

The server resolves paths from the owning agent's home. Clients send logical
agent and app IDs, not filesystem paths. App discovery skips invalid entries;
an existing agent without an `apps/` directory has an empty app list.

<a id="browser-sdk"></a>

## Browser SDK

Load the runtime-provided script with `<script src="holon.js"></script>` from
the app's root entry page. It exposes `window.Holon`.

<a id="context"></a>

### `context()`

Returns a promise containing the SDK version, owning agent, app, and session:

```json
{
  "sdk_version": "1",
  "agent_id": "main",
  "app_id": "ping-card",
  "session": { "authenticated": true }
}
```

Successful access does not make app input operator instructions.

<a id="requestrequesttype-payload-requestid"></a>

### `request(requestType, payload?, requestId?)`

Posts a versioned request to the app's `request` endpoint. The response contains
`ok`, `version`, `request_id`, `agent_id`, `app_id`, `status`, and `message_id`.
`status: "accepted"` means the message entered the queue. It does not report
business completion or guarantee that the agent will carry out the request.

Requests retain webhook provenance with source `app:<app-id>`. A request type
such as `ping` is application data; it is not a registered runtime command.

<a id="eventsoptions"></a>

### `events(options?)`

Returns an async iterable of app events. Options are `signal` (an
`AbortSignal`) and `lastEventId` (a sequence string for resuming). Breaking out
of iteration or aborting closes the stream.

Each event contains `version`, `agent_id`, `app_id`, and an `event` object with
`sequence`, `type`, `timestamp`, `message_id`, and an optional `request_id`.
The current stream reports `message_enqueued` and
`message_processing_started` for requests from this app. It is **not a stream
of completed results**, and it does not expose all agent activity.

Replay uses a bounded recent-event window. After a disconnect, verify work
state rather than treating a missing event as evidence that nothing happened.

<a id="hosting-and-access-limits"></a>

## Hosting and access limits

- Ordinary published files are served without an extension allowlist.
  `.wasm` uses `application/wasm`; `.data`, unknown types, and files without
  extensions use `application/octet-stream`. Serving a script file does not
  execute it on the server.
- Individual files, including entry pages, default to a 64 MiB
  (`67108864` bytes) limit. The complete file must fit even for range requests;
  exceeding the limit returns `413` / `app_asset_too_large`. A file exactly
  at the limit is allowed. Manifest parsing retains its separate 64 KiB bound.
- Canonicalized paths must remain inside the selected app root. Traversal,
  symlink escapes, and hidden path components such as `.env` and `.git` are
  rejected. There are no directory listings. Publish only intended assets.
- Apps use the runtime's authentication and share its origin. A remote browser
  needs an authenticated session; embedding a bearer token in app files is
  unsafe.
- By default the platform sends no App `Content-Security-Policy` header.
  An operator may supply a complete policy; app meta CSP and browser CORS
  remain effective. A configured policy is not a third-party-app sandbox.
- Apps do not get a separate origin, sandboxed iframe, or isolation from the
  host's ambient session credentials. Host only app code you trust.

### Daemon configuration

`local_apps` is a daemon-wide configuration object, not a manifest field:

| Key | Default | Validation |
|---|---|---|
| `local_apps.max_asset_bytes` | `67108864` | Positive integer byte count; zero is not unlimited. |
| `local_apps.csp` | Unset (`null`) | Complete HTTP header value; invalid values are rejected. |

Configure a 128 MiB limit and an optional same-origin policy:

```sh
holon config set local_apps.max_asset_bytes 134217728
holon config set local_apps.csp "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'"
```

This policy is an example, not a default or a Unity compatibility guarantee.
Use `holon config unset local_apps.max_asset_bytes` or
`holon config unset local_apps.csp` to restore the corresponding default.
With a running daemon, `config set` and `config unset` use the existing
configuration API to persist changes and schedule configuration reload.
After a successful reload, subsequent App requests use the new settings;
in-flight streams retain the policy already acquired for their request.
The CLI does not silently fall back to offline writes when an update is
rejected. Direct configuration-file edits do not trigger reload automatically.
Offline settings take effect when the daemon starts or restarts.
Apps cannot override these settings. Web GUI and Explorer size/CSP/sandbox
policies are unchanged.

### Transfer semantics and capacity

Entry pages and assets support `GET`, `HEAD`, single `Range` requests
(`206`/`416`), `ETag`, `304`, and `If-Range`. `HEAD` ignores `Range`,
sends no body, and does not read file content. Responses retain
`Cache-Control: no-store` and `X-Content-Type-Options: nosniff`; validators
do not promise persistent caching.

Files stream in 64 KiB chunks rather than being buffered whole, with reads
capped at the checked response length. File growth cannot enlarge the response;
truncation or I/O errors interrupt the stream. App and Explorer share this
transport code, not authorization or publication policy.

Each HTTP `AppState` shares 16 asset-stream permits across all agents and apps.
When all are held, another asset request returns `503` / `app_asset_busy`
with `Retry-After: 1`. Body completion, errors, or dropping the response release
the permit; bodyless responses release it promptly. SDK and API endpoints do
not consume these permits. The stream-buffer budget is not a process RSS cap.

### Unity and deployment scope

The initial target is uncompressed, single-threaded Unity Web output.
`.gz`, `.br`, and `.unityweb` are downloadable as ordinary files, but that
does not promise runnable precompressed builds or correct original
MIME/`Content-Encoding` negotiation. Multithreading, COOP/COEP,
SharedArrayBuffer enablement, reverse proxying, and server-side execution are
not added.

Real-game and real Mac/Safari acceptance have not been completed. Automated
HTTP checks are not evidence of those results. Existing deployments need an
updated binary and configuration activation; this documentation does not
deploy or restart a running service.

The [HTTP control plane](http-control-plane.md) owns the route and
authentication reference. See [Execution and access boundaries](../concepts/security-and-execution-boundaries.md)
before exposing a runtime remotely.
