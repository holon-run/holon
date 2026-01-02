# TypeScript Claude Agent (prototype)

This agent mirrors the Python bridge behavior using the Claude Agent SDK (v1) with the Claude Code runtime.

Notes:
- Entrypoint is `node /app/dist/agent.js`.
- Uses the Agent SDK `query()` stream and parses SDK messages for artifacts.
- Model overrides: `HOLON_MODEL`, `HOLON_FALLBACK_MODEL`.
- Timeouts: `HOLON_QUERY_TIMEOUT_SECONDS`, `HOLON_HEARTBEAT_SECONDS`, `HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS`, `HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS`.
- SDK logging: The Anthropic SDK log level is automatically configured based on Holon's log level (debug/info enable SDK logging, progress/minimal do not). Override with `HOLON_ANTHROPIC_LOG` or `ANTHROPIC_LOG` env vars for explicit control (e.g., `debug`, `info`, `warn`, `error`).

## Agent bundle build
Build a bundle that follows `docs/agent-bundle.md`:

```
npm run bundle
```

Defaults to writing the bundle under `agents/claude/dist/agent-bundles`.

Environment variables:
- `BUNDLE_OUTPUT_DIR`: override output directory.
- `BUNDLE_NAME`, `BUNDLE_VERSION`: bundle identity.
- `BUNDLE_PLATFORM`, `BUNDLE_ARCH`, `BUNDLE_LIBC`: target metadata.
- `BUNDLE_NODE_VERSION`: runtime version recorded in the manifest (recommended).
- `BUNDLE_ENGINE_NAME`, `BUNDLE_ENGINE_SDK`, `BUNDLE_ENGINE_SDK_VERSION`: override engine metadata in the manifest.

## Bundle verification
Run a smoke test that builds a bundle and runs the agent in probe mode using a Node base image.

```
npm run verify-bundle
```

The verification uses a dummy API endpoint to avoid real Claude requests and checks that
`/holon/output/manifest.json` is written.
