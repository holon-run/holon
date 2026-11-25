# holon-acp

Vendor-neutral client for the [Agent Client Protocol](https://github.com/holon-run/holon/blob/main/docs/rfcs/external-acp-agent-harness.md)
(ACP, wire v1) plus declarative external harness profiles. The first profile is
Google Antigravity (`agy acp`).

The crate is intentionally independent of `holon-runtime`: it can be compiled
and tested on its own, and the runtime integrates it later as an `AcpTurnDriver`
(see issue #4532 and parent tracker #4530).

## Scope

- `stdio` single-stream framing: NDJSON JSON-RPC 2.0 encode/decode and a
  bidirectional multiplexer. Only protocol frames use the stream; diagnostics
  belong on `stderr` of the harness process.
- Client methods: `initialize` (protocol version 1, trimmed default client
  capabilities), `session/new`, `session/resume`, `session/load`,
  `session/prompt` (streams `session/update` until `stopReason`), and
  `session/cancel`.
- `session/resume` and `session/load` preflight the peer's handshake capability
  declarations and never call an undeclared method.
- Reverse requests (`session/request_permission`, `elicitation/create`) are
  routed to an `AcpClientHandler`. Cancellation resolves any pending reverse
  request so the two directions cannot deadlock.
- Declarative `AcpHarnessSpec` registry and the built-in `antigravity` profile:
  `--sandbox`, `--add-dir`, `--agent <name>`, `--log-file`,
  `--disable-telemetry`, session `Model` / `Thinking level` / mode options, and
  structured mapping of JSON-RPC `-32000` / `-32002`.

## Non-goals

- No scheduler or Turn-loop change in `holon-runtime`.
- No client-side `fs.*` or `terminal/*` executors.
- No remote HTTP/WebSocket ACP transport.
