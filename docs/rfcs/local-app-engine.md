---
title: RFC: Local App Engine
date: 2026-09-28
status: accepted
handle: rfc-local-app-engine
---

# RFC: Local App Engine

## Summary

Holon can host small HTML and JavaScript applications owned by an Agent.
The first slice is intentionally a local static-app engine: Holon discovers
apps in an Agent's `agent_home/apps/` directory, validates a manifest, and
serves the entry document and a bounded set of static assets.

The application URL is logical. Clients provide an `agent_id` and `app_id`;
they never provide a filesystem path.

## Storage and discovery

An app is stored at:

```text
<data_dir>/agents/<agent_id>/apps/<app_id>/
```

The directory must contain `manifest.json`. `GET /apps/{agent_id}` returns
only apps with a valid app directory and valid manifest. A valid Agent with a
missing or empty `apps/` directory returns an empty list; an unknown Agent
returns `404`. Discovery does not create
directories. Invalid entries are skipped during discovery and are reported as
errors when addressed directly.

The manifest has this first-slice shape:

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
be a non-empty safe relative path within the app root.

## HTTP contract

The hosted app surface is mounted at `/apps` and uses the existing session
authentication boundary:

- `GET /apps/{agent_id}` — discover valid apps owned by the Agent.
- `GET /apps/{agent_id}/{app_id}` or `/` — serve the manifest entry document.
- `GET /apps/{agent_id}/{app_id}/{asset_path}` — serve an app asset.

Entry documents and assets are served with `Cache-Control: no-store`,
`X-Content-Type-Options: nosniff`, and the app CSP. The first slice allows
HTML, JavaScript, CSS, JSON, text, SVG, common image formats, and common font
formats. Unknown extensions are rejected rather than served as executable or
opaque content.

Manifest files and assets are bounded in size. The current limits are 64 KiB
for `manifest.json` and 8 MiB for an individual asset.

## Security invariants

Every route validates `agent_id`, `app_id`, and relative asset components.
Empty values, control characters, whitespace, path separators, `..`, and
overlong segments are rejected. Server-side Agent state resolves the Agent
home; the request cannot select an arbitrary root.

Before serving an app or asset, the resolved path is canonicalized and checked
to remain under the owning `apps/` directory. This check also prevents
the `apps/` root itself must not be a symlink, and symlinks from escaping the
app root are rejected. Asset reads require a regular file and an
allowlisted content type.

The baseline CSP keeps scripts, styles, images, fonts, and connections on the
same origin (with data URLs only for images and fonts), disallows a base URI,
and limits framing and form actions to the same origin. This permits the
same-origin SDK surface planned for #3255.

## Trust boundary and non-goals

Hosted apps are same-origin with the control plane. An app can therefore make
authenticated same-origin requests permitted by the existing session boundary;
the first slice does not provide an app-specific token scope or a separate
origin. `HttpOnly` session cookies are not readable through
`document.cookie`, but same-origin browser requests can still carry ambient
session authentication.

The following are intentionally deferred:

- complex cross-Agent ACLs;
- strong sandbox or iframe isolation;
- a separate app origin;
- a complete capability or permission system;
- dynamic server-side app execution;
- route inventory integration for nested `/apps` routes.

These boundaries must be revisited before exposing untrusted multi-user apps.
