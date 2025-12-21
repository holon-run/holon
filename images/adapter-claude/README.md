# TypeScript Claude Adapter (prototype)

This adapter mirrors the Python bridge behavior using the Claude Code CLI directly from Node/TypeScript.

Notes:
- Entrypoint is `node /app/dist/adapter.js`.
- Uses `claude --output-format stream-json --print` and parses stream JSON for artifacts.
- Model overrides: `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`.
- Timeouts: `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS`.
- Current CLI invocation passes prompts on the command line, which can hit OS command length limits for very large prompts; for large inputs, prefer passing the prompt via stdin (e.g. piping from a file) or by referencing a prompt file instead of embedding the full text directly in the command.
