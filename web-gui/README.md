# Holon Web GUI

This directory contains the first local Web GUI design assets for Holon:

- `DESIGN.md` — the visual and interaction contract, written in a
  design.md-style format with machine-readable tokens and human-readable rules.
- `prototype/` — a static, clickable prototype for the first GUI shape.

The prototype is intentionally dependency-free. It is a review artifact and an
implementation seed, not the production Web app yet.

## Preview locally

From the repository root:

```bash
python3 -m http.server 4173 --directory web-gui/prototype
```

Then open:

```text
http://127.0.0.1:4173/
```

You can also open `web-gui/prototype/index.html` directly in a browser, but the
local server path better matches how the UI will eventually be served by
`holon serve`.

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

- production API calls
- authentication/token handling
- bundling into the `holon` binary
- Tauri desktop shell
- full responsive/mobile interaction

Those should be added after the layout and information architecture are
reviewed.
