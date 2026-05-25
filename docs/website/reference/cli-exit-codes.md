---
title: CLI exit codes
summary: Exit-code and stream-routing contract for Holon's command-line interface.
order: 13
---

# CLI exit codes

This page defines the current exit-code contract for `holon` commands. It is a
script-facing companion to the [CLI stability policy](./cli-stability-policy.md)
and [CLI contract inventory](./cli-contract-inventory.md).

Holon keeps the contract intentionally small while the runtime is pre-1.0:

| Exit code | Meaning | Stream contract |
|---:|---|---|
| `0` | The CLI command completed successfully. | Machine-readable commands write their JSON or documented raw response to stdout. Human commands may write prose to stdout. Stderr remains diagnostic/log output. |
| `1` | The command was accepted by the parser, but execution failed before a successful CLI result was produced. This includes unreachable control-plane requests, invalid runtime/config/provider setup, failed file IO, malformed stored config, and HTTP/control-plane errors surfaced by the client. | Stdout is not script-safe and should usually be empty. Diagnostics are written to stderr by the top-level error renderer and may include chained context. |
| `2` | Clap rejected the invocation before command execution, for example an unknown flag, missing required argument, invalid enum value, or invalid numeric range. | Stdout is empty. Clap writes usage/error text to stderr. |

Codes outside this table are not part of Holon's stable CLI contract. In
particular, POSIX signal-derived exit statuses are owned by the operating system
and should not be interpreted as Holon business results.

## Representative cases

### Invalid arguments

Parser failures exit with code `2`:

```bash
holon --definitely-not-a-holon-flag
echo $? # 2
```

Use this class for invocation-shape problems only. Scripts should treat stderr
as human-readable help/error text, not as a machine-readable schema.

### Unreachable control plane

Commands that need the local or remote control plane exit with code `1` when
transport fails:

```bash
HOLON_HTTP_ADDR=127.0.0.1:9 holon status
echo $? # 1
```

The CLI does not synthesize a JSON error envelope on stdout for transport
failures. Use stderr for operator diagnostics and retry/backoff decisions in the
calling script.

### Invalid configuration or provider setup

Configuration that parses as a CLI invocation but is rejected by Holon's config
validators exits with code `1`:

```bash
holon config providers set script-test \
  --transport openai_responses \
  --base-url not-a-url
echo $? # 1
```

Commands that validate stored config during startup also use code `1` for
malformed or unsupported config.

### Successful transport with failed business state

When a command successfully talks to the control plane, Holon's CLI exit code
reports the command transport/result-rendering outcome, not every business
state embedded in the returned JSON. For example, a command that successfully
creates, inspects, or returns a task record exits `0` if the HTTP request and
response rendering succeeded, even if the returned task or runtime object has a
domain state such as `failed`, `cancelled`, `blocked`, or `waiting`.

Scripts must inspect the documented JSON fields for business state. Holon will
only promote a business state to a non-zero process exit when a command's
specific reference page documents that behavior.

## Stdout and stderr

- Treat stdout as machine-readable only for commands whose reference or
  inventory row says they emit JSON or a documented raw response body.
- Treat stderr as human diagnostics. It may contain Clap usage text, anyhow
  context chains, tracing logs, provider diagnostics, or operating-system
  errors.
- Do not parse exact stderr prose for stable automation. Prefer JSON output or
  HTTP/API error envelopes when a stable machine contract is required.
