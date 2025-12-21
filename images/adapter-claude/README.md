# TypeScript Claude Adapter (prototype)

This adapter mirrors the Python bridge behavior using the Claude Agent SDK (v1) with the Claude Code runtime.

Notes:
- Entrypoint is `node /app/dist/adapter.js`.
- Uses the Agent SDK `query()` stream and parses SDK messages for artifacts.
- Model overrides: `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`.
- Timeouts: `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS`.
