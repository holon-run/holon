---
title: CLI stability policy
summary: Support policy for Holon's command-line surfaces and machine-readable output contracts.
order: 12
---

# CLI stability policy

Holon is still pre-1.0, but not every CLI surface has the same change risk.
This policy explains which command-line surfaces are safe for scripts, which
ones are primarily human/debug aids, and what bar future command changes should
meet.

Use this page with:

- [CLI reference](./cli.md) for the current command tree and common workflows.
- [CLI contract inventory](./cli-contract-inventory.md) for per-command
  stability levels, output modes, and known contract gaps.
- [CLI exit codes](./cli-exit-codes.md) for process exit codes and stdout/stderr
  routing.
- [API contract inventory](./api-contract-inventory.md) for HTTP response
  shapes that many CLI JSON outputs mirror.

## Stability levels

| Level | Intended use | Support policy |
|-------|--------------|----------------|
| `stable` | Public CLI contract that users and scripts may depend on. | Avoid breaking changes. If a breaking change is unavoidable, keep a documented migration path and call it out in release notes. |
| `experimental` | Publicly reachable surface that is still being shaped. | May change while Holon's runtime model stabilizes. Prefer warnings, aliases, or compatibility output before removal when practical. |
| `internal` | Debug, fixture, local-development, or runtime-inspection surface. | Not intended for external automation. May change with implementation details. |
| `deprecated` | Compatibility surface with a documented replacement. | Keep working until the documented compatibility window or removal criteria are met. Do not add new automation against it. |

When a command has mixed output modes, apply the level to the specific surface
you consume. For example, a command path may be a stable candidate while its
human-readable prose remains experimental.

## Script-safe surfaces

Scripts should prefer surfaces that have all of these properties:

1. A documented command path and flag set in the CLI reference or contract
   inventory.
2. Machine-readable JSON output, preferably emitted through Holon's normalized
   JSON printing path rather than a raw HTTP response passthrough.
3. A documented response owner, either in the CLI contract inventory or the
   HTTP/API inventory when the CLI mirrors a control-plane response.
4. Explicit exit-code behavior from the
   [CLI exit-code contract](./cli-exit-codes.md).

Current script-facing candidates include:

- `holon daemon status`, `holon daemon logs`, and the daemon lifecycle commands.
- `holon config get|set|unset|schema` and credential/provider management
  commands that print JSON.
- `holon status`, `holon agent list`, and `holon agent status`.
- `holon workspace attach|exit|detach`, subject to workspace identity contract
  stability.
- `holon run --json` and `holon solve --json` only for the documented response
  shape; their human output remains for operators.

Scripts should avoid parsing exact help text, tracing logs, debug prose, or
human summaries. Those outputs are for people and may change to improve clarity.

## Human and diagnostic surfaces

The following surfaces are intentionally not primary automation contracts:

- `holon --help` and `holon <command> --help` prose. Command and flag names are
  contract material; formatting and explanatory text are not.
- `holon run` and `holon solve` default human output.
- `holon serve` startup summaries and logs.
- `holon debug *` commands.
- `stderr`, including tracing logs, Clap errors, credential prompts, and
  provider/runtime diagnostics.

If a diagnostic surface becomes important for automation, promote the needed
fields into a JSON response or a documented API/CLI contract instead of parsing
debug text.

## Deprecated `holon control`

`holon control` is deprecated. Use the agent lifecycle commands instead:

| Deprecated command | Replacement |
|--------------------|-------------|
| `holon control start --agent <AGENT>` | `holon agent start <AGENT>` |
| `holon control stop --agent <AGENT>` | `holon agent stop <AGENT>` |
| `holon control abort --agent <AGENT>` | `holon agent abort <AGENT>` |

Compatibility policy:

- Keep `holon control start|stop|abort` reachable through the 0.x line unless a
  release note announces a narrower removal window.
- Do not add new options or behavior only to `holon control`; improvements
  should land on `holon agent ...` first.
- Before removal, the replacement commands must have equivalent documented
  behavior for lifecycle state changes and exit/error reporting.
- Removal should happen only after the CLI contract inventory records the
  removal criteria and a release note has pointed users to the replacement.

## Change requirements

When changing CLI behavior:

1. Update [CLI reference](./cli.md) when command paths, flags, or common
   workflows change.
2. Update [CLI contract inventory](./cli-contract-inventory.md) when stability
   classification, output mode, or script-safety changes.
3. Update [API contract inventory](./api-contract-inventory.md) when CLI output
   mirrors a changed control-plane response.
4. Add or update tests for stable and stable-candidate command shape, output,
   or exit-code behavior.
5. Avoid introducing a new raw-output path for automation. If raw passthrough is
   necessary, document it as experimental until normalized.

Stable CLI surfaces should become boring: named explicitly, tested, and
documented where users will look before writing scripts.
