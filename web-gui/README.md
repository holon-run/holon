# Holon Web GUI

This directory contains the first local Web GUI design assets for Holon:

- `DESIGN.md` — the visual and interaction contract, written in a
  design.md-style format with machine-readable tokens and human-readable rules.
- `prototype/` — a static, clickable prototype for the first GUI shape.
- `app/` — a standalone Vite/React/TypeScript Web app that can call local
  Holon HTTP interfaces.

The prototype is intentionally dependency-free. It remains a review artifact
and implementation seed. Production UI work should happen in `app/` and keep
the prototype available as the visual/information-architecture baseline.

## Preview the static prototype locally

From the repository root:

```bash
python3 -m http.server 4173 --directory web-gui/prototype
```

Then open:

```text
http://127.0.0.1:4173/
```

You can also open `web-gui/prototype/index.html` directly in a browser, but the
local server path better matches how the UI may eventually be served by
`holon serve`.

## Run the standalone app

Install dependencies and run the Vite dev server:

```bash
cd web-gui/app
npm install
npm run dev
```

By default the app uses fixture fallback data so it can be reviewed without a
running Holon server.

To point it at the default local Holon HTTP server through the Vite dev proxy,
set `VITE_HOLON_API_BASE` to the same-origin proxy prefix:

```bash
cd web-gui/app
VITE_HOLON_API_BASE=/holon-api npm run dev
```

The dev proxy forwards `/holon-api/*` to `http://127.0.0.1:7878/*` by default.
If Holon is listening on a different local endpoint, override the proxy target:

```bash
cd web-gui/app
HOLON_API_PROXY_TARGET=http://127.0.0.1:<holon-port> VITE_HOLON_API_BASE=/holon-api npm run dev
```

Direct absolute API URLs such as
`VITE_HOLON_API_BASE=http://127.0.0.1:<holon-port>` are still supported by the
client, but local browser debugging may be blocked unless the Holon HTTP server
also sends matching CORS headers.

Build/check commands:

```bash
cd web-gui/app
npm run typecheck
npm run build
```

## Current standalone app scope

The standalone app currently implements:

- a Dashboard runtime overview and agent roster;
- an Agent conversation page with Info/Verbose/Debug display levels;
- fixture fallback when no local Holon endpoint is configured or reachable;
- direct reuse of existing Holon routes only.

The app intentionally remains independent from `holon serve`; it is not bundled
into the Rust binary yet.

## Existing Holon interfaces used

Dashboard uses:

- `GET /handshake`
- `GET /agents/list`
- `GET /agents/:id/state`
- `GET /agents/:id/briefs?limit=1`

Agent conversation uses:

- `GET /agents/list`
- `GET /agents/:id/state`
- `GET /agents/:id/briefs?limit=5`
- `GET /agents/:id/transcript?limit=40`
- `GET /agents/:id/events?limit=20&order=desc&max_level=verbose`

No GUI-specific backend routes are introduced in this branch.

## Current implementation constraints

Constraints for the current worktree:

- one logical work item per commit;
- reuse existing Holon interfaces only;
- do not embed into `holon serve` yet;
- keep Search and Settings lightweight until Dashboard and Agent conversation
  are useful;
- refer to the TUI when exposing display levels, runtime activity, and
  provenance.

## Known API gaps / follow-up candidates

The current app can function with existing routes, but these gaps may deserve
follow-up issues before a production Web GUI:

1. **CORS / local auth policy for standalone browser clients.** The Vite app
   calls the Holon HTTP server from a different origin during development. If
   the current server is not browser-accessible in a given mode, document or add
   an explicit local development policy rather than special-casing the GUI.
2. **Conversation-oriented projection.** The frontend currently merges briefs,
   transcript entries, and runtime events. A future contract may expose a
   projection that preserves provenance while reducing client-side heuristics.
3. **Send-message control path.** The composer is visual only for now. Wiring it
   to `POST /control/agents/:id/prompt` should happen after local auth and
   operator-authority UX are explicit.
4. **Inspector object details.** The current inspector shows WorkItem context
   from agent state. Rich task output, source refs, file previews, and event
   payload drill-downs can continue to use existing endpoints where available,
   but may need separate product decisions.

## Current prototype scope

The first prototype now demonstrates:

- a Codex-density but Holon-specific agent runtime workbench
- a global Dashboard focused on the full agent roster
- left-side global navigation only: Dashboard, Search, and Settings
- a bottom local/remote runtime connection status and active-agent quick switcher
- an agent conversation page with a collapsible left nav and optional right object inspector
- side panel examples for WorkItem detail, diff, file, web, and runtime evidence
- Holon work-spine state, Info/Verbose/Debug display levels, origin markers, and tool activity evidence
- a static fixture based on real `holon-pm` RuntimeDb rows

It uses static sample data that mirrors current Holon runtime concepts:
agents, the single conversation per agent, WorkItems, waits, tasks, worktrees,
briefs, origins, and event streams.
The current fixture was extracted from the local RuntimeDb at
`/Users/jolestar/.holon/state/runtime.sqlite`, scoped to `holon-pm` turns
915-919 and WorkItem `work_ad345d7d32bc92d`.

## Not in scope yet

- authentication/token handling
- bundling into the `holon` binary
- Tauri desktop shell
- full responsive/mobile interaction

Those should be added after the layout and information architecture are
reviewed.

Production API calls are in scope for the standalone app only when they can use
existing local Holon interfaces. New backend routes should be tracked as
follow-up API-gap issues rather than implemented in this prototype branch.
