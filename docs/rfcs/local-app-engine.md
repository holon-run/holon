---
title: RFC: Local App Engine
date: 2026-09-28
status: accepted
handle: rfc-local-app-engine
---

# RFC: Local App Engine

## Summary

Holon hosts static applications owned by an Agent. It discovers apps in an
Agent's `agent_home/apps/` directory, resolves optional manifest metadata,
and serves entry documents and ordinary published files through bounded
streaming responses. Uncompressed, single-threaded Unity Web builds are in
scope; this is not a server-side application engine.

The application URL is logical. Clients provide an `agent_id` and `app_id`;
they never provide a filesystem path.

## Storage and discovery

An app is stored at:

```text
<data_dir>/agents/<agent_id>/apps/<app_id>/
```

`GET /apps/{agent_id}` returns apps with a valid app directory and resolved
metadata. A valid Agent with a
missing or empty `apps/` directory returns an empty list; an unknown Agent
returns `404`. Discovery does not create
directories. Invalid entries are skipped during discovery and are reported as
errors when addressed directly.

Only when the root `manifest.json` directory entry is absent, metadata is
derived in memory: `id` and `name` equal the app directory name, `version`
is `"0.0.0"`, `entry` is `"index.html"`, and `description` is `null`.
The default entry must be a readable regular file contained in the app root.
No recursive entry guessing, directory creation, or manifest write occurs.
The version is an undeclared-version placeholder, not a release or cache
fingerprint. A request for a missing `manifest.json` still returns `404`;
there is no virtual manifest asset.

An explicit manifest has this shape:

```json
{
  "id": "example",
  "name": "Example App",
  "version": "1.0.0",
  "entry": "index.html",
  "description": "Optional description"
}
```

`id`, `name`, `version`, and `entry` are required. `id` must equal the
requested `app_id`; `name` and `version` must be non-empty; and `entry` must
be a non-empty safe relative path to an HTML file within the app root.
Explicit manifests retain their existing required fields and custom entry
behavior; missing explicit entry files still return `404` on access.
Invalid JSON, invalid fields, unreadable files, directories, dangling or
escaping symlinks, and manifests larger than 64 KiB are errors, never reasons
to fall back to `index.html`. Contained manifest symlinks must still resolve
to readable regular files within that bound. Manifest reads themselves are
bounded independently of the asset limit.

## HTTP contract

The hosted app surface is mounted at `/apps` and uses the existing session
authentication boundary:

- `GET /apps/{agent_id}` — discover valid apps owned by the Agent.
- `GET /apps/{agent_id}/{app_id}` or `/` — serve the manifest entry document.
- `GET /apps/{agent_id}/{app_id}/context` — return the App SDK context.
- `POST /apps/{agent_id}/{app_id}/request` — enqueue an App request.
- `GET /apps/{agent_id}/{app_id}/events` — stream App-originated lifecycle events.
- `GET /apps/{agent_id}/{app_id}/holon.js` — serve the built browser SDK artifact.
- `GET /apps/{agent_id}/{app_id}/{asset_path}` — serve an app asset.

Entry documents and assets support `GET` and `HEAD`, single byte ranges
(`206`/`416`), `ETag`, `304`, and `If-Range`. `HEAD` ignores `Range` and
does not read a response body. Responses retain `Cache-Control: no-store`
and `X-Content-Type-Options: nosniff`; validators do not imply durable caching.
All ordinary published files are eligible, not just an extension allowlist.
MIME describes content rather than authorizing access: `.wasm` uses
`application/wasm`, while `.data`, extensionless files, and unknown types
use `application/octet-stream`.

### Daemon policy

The persisted `local_apps` settings are daemon-wide, not per-app manifest
overrides:

- `max_asset_bytes`: positive integer, default `67108864` (64 MiB).
  Zero does not mean unlimited. The whole opened file is checked before
  successful headers, including for ranges. Oversized files return
  `413` / `app_asset_too_large`; files exactly at the limit are allowed.
- `csp`: optional complete `Content-Security-Policy` header string, validated
  as an HTTP header value. Unset means no platform App CSP header. An
  invalid value is rejected, not silently ignored. An app's own meta CSP
  remains subject to browser enforcement.

Use `holon config set local_apps.max_asset_bytes <bytes>` and
`holon config set local_apps.csp '<policy>'`; `holon config unset` restores
each default. With a running daemon, these commands use its existing
configuration API to persist changes and schedule configuration reload.
After a successful reload, subsequent App requests read the new settings;
in-flight streams retain the policy already acquired for their request.
The CLI does not silently fall back to offline writes if an update is rejected.
Direct configuration-file edits do not trigger reload automatically.
Offline configuration takes effect when the daemon starts or restarts.
Changing Local App policy does not alter Web GUI or Explorer policy.

### Shared file transport and capacity

App and Explorer responses share a narrow Axum/Hyper `file_response`
module. Callers retain authentication, root authorization, MIME, cache,
CSP, download, and size policies; sharing transport does not expose
Explorer directory listings or workspace roots to apps. Explorer's existing
text-preview limit and sandbox remain unchanged.

The module streams an already opened regular file in 64 KiB chunks, capped
at the checked response length with `take`. It does not buffer an entire
asset in memory. File growth cannot extend the response; truncation or
midstream I/O failure interrupts it instead of fabricating complete success.
This does not eliminate all concurrent-file-mutation or TOCTOU risks.

Each HTTP `AppState` shares 16 asset-stream permits across all agents and
apps. A seventeenth concurrent transfer returns `503` / `app_asset_busy`
with `Retry-After: 1`, without indefinite queuing. A permit is held by the
body until completion, error, or drop; bodyless responses release it promptly.
SDK and API endpoints do not consume asset permits. The 16 × 64 KiB stream
buffer budget is not a hard limit on process RSS.

## Security invariants

Every route validates `agent_id`, `app_id`, and relative asset components.
Empty values, control characters, whitespace, path separators, `..`, and
overlong segments are rejected. Server-side Agent state resolves the Agent
home; the request cannot select an arbitrary root.

Before serving an app or asset, the resolved path is canonicalized and checked
to remain under the owning `apps/` directory and the selected app root.
The `apps/` root itself must not be a symlink; app-root and asset symlink
escapes are rejected. Hidden path components such as `.env` and `.git` are
rejected. Reads require regular files; there are no directory listings or
server-side script execution. App directories must contain only intended
publication artifacts.

Local Apps have no platform CSP by default. Operators may configure a
complete policy for trusted apps; neither that policy nor optional manifest
metadata is a sandbox or an authority grant. Authentication, bootstrap,
origin checks, and App request provenance remain unchanged.

The browser SDK is built from `packages/app-sdk` and exposes
`window.Holon.context()`, `window.Holon.request()`, and `window.Holon.events()`.
The Rust route embeds `dist/holon.js` when available and returns an explicit
service-unavailable response when the optional artifact is absent; it never
reconstructs the SDK JavaScript in Rust source.

## Trust boundary and non-goals

Hosted apps are same-origin with the control plane. An app can therefore make
authenticated same-origin requests permitted by the existing session boundary;
the first slice does not provide an app-specific token scope or a separate
origin. `HttpOnly` session cookies are not readable through
`document.cookie`, but same-origin browser requests can still carry ambient
session authentication.

The following are intentionally deferred:

- precompressed Unity delivery: `.gz`, `.br`, and `.unityweb` are served as
  ordinary files, without a promise of correct original MIME/encoding
  negotiation or runnable compressed builds;
- multithreaded Unity, COOP/COEP, and SharedArrayBuffer enablement;
- complex cross-Agent ACLs;
- strong sandbox or iframe isolation;
- a separate app origin;
- a complete capability or permission system;
- dynamic server-side app execution;
- route inventory integration for nested `/apps` routes.

These boundaries must be revisited before exposing untrusted multi-user apps.
Cross-origin resources remain subject to browser CORS, application behavior,
and any configured CSP; they are not universally blocked by a default App CSP.

Real-game acceptance and real Mac/Safari acceptance have not been completed.
Automated HTTP checks do not replace those checks. Existing deployments need
the updated binary; configuration activation follows the reload/startup
semantics above. This task does not deploy or restart production services.
