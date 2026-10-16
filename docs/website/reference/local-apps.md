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

Every app needs a `manifest.json` no larger than 64 KiB.

| Field | Required | Contract |
|---|---|---|
| `id` | Yes | Must match the app directory name and requested app ID. |
| `name` | Yes | Nonempty display name. |
| `version` | Yes | Nonempty version string. |
| `entry` | Yes | Safe, nonempty relative path to an HTML file inside the app root. |
| `description` | No | Short description. |

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

- Individual assets are limited to 8 MiB and must use a supported static file
  type: HTML, JavaScript, CSS, JSON, text, SVG, common images, or fonts.
- Canonicalized paths must remain inside the app root. Traversal and symlink
  escapes are rejected.
- Apps use the runtime's authentication and share its origin. A remote browser
  needs an authenticated session; embedding a bearer token in app files is
  unsafe.
- The baseline Content Security Policy permits same-origin resources and
  connections, inline scripts/styles, and `data:` images/fonts. External
  scripts and cross-origin network calls are blocked.
- Apps do not get a separate origin, sandboxed iframe, or isolation from the
  host's ambient session credentials. Host only app code you trust.

The [HTTP control plane](http-control-plane.md) owns the route and
authentication reference. See [Execution and access boundaries](../concepts/security-and-execution-boundaries.md)
before exposing a runtime remotely.
