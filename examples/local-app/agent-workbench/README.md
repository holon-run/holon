# Agent Workbench

This is a minimal Local App Engine reference app. It is intentionally only
HTML, CSS, JavaScript, and the hosted `holon.js` SDK:

- `manifest.json` declares the app entry point.
- `index.html` displays Agent context and request/event status.
- `app.js` calls `Holon.context()`, `Holon.request()`, and `Holon.events()`.
- `styles.css` provides presentation only.

Copy this directory to the owning Agent's
`<data_dir>/agents/<agent_id>/apps/agent-workbench/` directory, then open:

```text
/apps/<agent_id>/agent-workbench/
```

The repeatable HTTP acceptance path is:

```bash
cargo test --test http_apps apps_reference_workbench_end_to_end
```

That test copies these exact files into an isolated AgentHome fixture and
verifies discovery, entry/resource loading, context, request/response, and
SSE event delivery. The reference app does not add server routes or require
another daemon.
