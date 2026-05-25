---
title: CLI contract inventory
summary: First-pass stability inventory for Holon's command-line parameters, outputs, and follow-up contract work.
order: 11
---

# CLI Contract Inventory

This inventory captures Holon's current CLI surface as compiled from
`src/main.rs` and checked against `target/debug/holon --help` for `holon 0.14.1`.
It is a stability planning document, not a promise that every listed command is
already stable.

The current implementation keeps the CLI in one Rust binary:

- `src/main.rs` defines the Clap command tree and most CLI handlers.
- `docs/website/reference/cli.md` is the user-facing command reference.
- `docs/website/reference/cli-stability-policy.md` documents the
  user-facing support policy for stable, experimental, internal, and
  deprecated CLI surfaces.
- `docs/website/reference/configuration.md` documents config files,
  credential-related environment variables, and diagnostics.

## Stability levels

See [CLI stability policy](./cli-stability-policy.md) for the user-facing
support policy behind these labels.

| Level | Meaning | Change policy |
|---|---|---|
| `stable` | Intended public surface that users and scripts may reasonably depend on. | Avoid breaking changes; require release notes and migration path. |
| `experimental` | Publicly reachable but still being shaped. | May change before 1.0; prefer warnings or aliases before removal. |
| `internal` | Debug, runtime, or local-development surface. | Not intended for external automation; may change when internals change. |
| `deprecated` | Kept for compatibility but replaced by another surface. | Document replacement and remove only through an explicit deprecation plan. |

## Cross-cutting CLI contract

| Surface | Current behavior | Initial stability | Notes |
|---|---|---:|---|
| Command parser | Clap-derived command tree with `--help` and `--version` at the root. | `stable` | Command and flag names are the highest-value CLI contract. |
| Help text | Human-readable Clap output. | `experimental` | Useful for users; exact spacing and prose should not be treated as machine-readable. |
| Errors | Clap validation errors or `anyhow` errors rendered by the binary runtime. | `experimental` | Exit code shape needs explicit tests before declaring stable. |
| JSON output | Script-facing JSON commands pretty-print JSON to stdout via the shared `print_json` path. | `experimental` | JSON field shape usually comes from runtime/control-plane structs and needs API inventory alignment. |
| Human output | `run`, `solve`, `serve`, `debug latency`, `debug prompt`, and some debug/export commands write human text to stdout. | `experimental` | Do not snapshot full prose unless the command is intentionally script-facing. |
| stderr | Tracing logs, Clap errors, credential prompt, and some provider/runtime diagnostics. | `experimental` | Credential prompt intentionally writes to stderr. |
| stdin | Only `config credentials set --stdin` currently reads from stdin. | `stable` candidate | Interaction details need a focused test. |
| Config/env | Most commands load `AppConfig`; config commands use `$HOLON_HOME` for offline config paths. | `experimental` | See configuration reference for the broader env surface. |

## Command inventory

### Root

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon --help` | none | `-h, --help`, `-V, --version` | human help to stdout | `stable` candidate | Should be covered by command-tree snapshot tests. |
| `holon <COMMAND> --help` | command-dependent | command-dependent | human help to stdout | `stable` candidate for shape; `experimental` for prose | Regenerate `cli.md` when command shape changes. |

### Server and daemon

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon serve` | none | `--access <local\|tunnel\|lan\|tailnet>` default `local`; `--host <HOST>`; `--listen <LISTEN>`; `--port <PORT>`; `--advertise <ADVERTISE>`; `--token <TOKEN>`; `--token-file <TOKEN_FILE>` | long-running server; startup summaries on stdout; logs/tracing on stderr | `experimental` | Non-loopback/tailnet/lan access requires control token via flag, file, or `HOLON_CONTROL_TOKEN`. |
| `holon daemon start` | none | same `ServeOptions` as `serve` | JSON daemon lifecycle response | `stable` candidate | Inline token is passed to child process via env, not argv. |
| `holon daemon stop` | none | none | JSON daemon lifecycle response | `stable` candidate | Uses local daemon lifecycle helper. |
| `holon daemon status` | none | none | JSON daemon status response | `stable` candidate | Important local inspection surface. |
| `holon daemon restart` | none | same `ServeOptions` as `serve` | JSON daemon lifecycle response | `stable` candidate | Same access/token validation as `serve`. |
| `holon daemon logs` | none | `--tail <TAIL>` default `80` | JSON daemon log response | `stable` candidate | `daemon logs` is documented as a local troubleshooting surface. |

### Offline configuration

These commands operate on persisted config or credential files directly rather
than requiring a running daemon.

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon config get` | `<KEY>` | none | JSON value for the key | `stable` candidate | Key set comes from the configuration contract. |
| `holon config set` | `<KEY> <VALUE>` | none | JSON value after write | `stable` candidate | Value parsing is key-specific. |
| `holon config unset` | `<KEY>` | none | JSON `{ "key": ..., "status": "unset" }` | `stable` candidate | Status string should be locked if scripts depend on it. |
| `holon config list` | none | none | full persisted config JSON | `experimental` | Exposes broad config file shape; align with config reference before declaring stable. |
| `holon config schema` | none | none | JSON config schema/metadata | `stable` candidate | Good machine-readable contract candidate. |
| `holon config doctor` | none | none | JSON provider/system diagnostics | `experimental` | Diagnostic shape may evolve as providers change. |
| `holon config models list` | none | none | JSON model availability list | `experimental` | Provider catalog and availability details are still evolving. |

### Provider configuration

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon config providers set` | `<PROVIDER>` | `--transport <TRANSPORT>`; `--base-url <BASE_URL>`; `--credential-source <SOURCE>` default `none`; `--credential-kind <KIND>` default `none`; `--credential-env <ENV>`; `--credential-profile <PROFILE>`; `--credential-external <COMMAND>` | JSON `{ "applied_via": "offline_store", "provider": ... }` | `stable` candidate for command shape; `experimental` for provider object | Built-in providers may reject incompatible transport overrides. |
| `holon config providers get` | `<PROVIDER>` | none | JSON provider view | `experimental` | Output uses runtime provider view. |
| `holon config providers list` | none | none | JSON array/object of provider views | `experimental` | Output shape should be reconciled with API/config inventory. |
| `holon config providers remove` | `<PROVIDER>` | none | JSON `{ "applied_via": "offline_store", "provider": ..., "status": "removed\|not_configured" }` | `stable` candidate | Status strings are script-facing. |
| `holon config providers doctor` | `<PROVIDER>` | none | JSON provider view plus model-chain diagnostics | `experimental` | Diagnostic details may evolve. |

### Credential configuration

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon config credentials set` | `<PROFILE>` | required `--kind <KIND>`; one of `--stdin` or `--material <MATERIAL>` | JSON `{ "applied_via": "offline_store", "credential": ... }` | `stable` candidate | `--stdin` prompt goes to stderr; raw `--material` is intentionally discouraged for secrets. |
| `holon config credentials list` | none | none | JSON credential profile list | `stable` candidate | Must not expose credential material. |
| `holon config credentials remove` | `<PROFILE>` | none | JSON `{ "applied_via": "offline_store", "credential": ... }` | `stable` candidate | Credential status shape needs explicit contract tests. |

### Agent interaction and inspection

These commands require a reachable local control plane unless noted otherwise.

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon prompt` | `<TEXT>` | `--agent <AGENT>` | JSON control prompt response | `experimental` | Lightweight prompt path; response shape belongs to control-plane API inventory. |
| `holon status` | none | `--agent <AGENT>` | JSON agent status | `stable` candidate | Agent summary is a key operator/API surface. |
| `holon tail` | none | `--limit <LIMIT>` default `20`; `--agent <AGENT>` | JSON recent briefs/log tail | `stable` candidate | Result shape should align with brief/output contract. |
| `holon transcript` | none | `--limit <LIMIT>` default `50`; `--agent <AGENT>` | JSON transcript entries | `stable` candidate | Transcript entry stability needs API inventory. |
| `holon task run` | `<SUMMARY>` | required `--cmd <CMD>`; `--workdir <WORKDIR>`; `--shell <SHELL>`; `--login <true\|false>`; `--tty`; `--yield-time-ms <MS>`; `--max-output-tokens <N>`; `--agent <AGENT>` | pretty JSON control-plane response | `experimental` | Creates command tasks through the control plane. |
| `holon task status` | `<TASK_ID>` | `--agent <AGENT>` | pretty JSON `TaskStatusSnapshot` | `experimental` | Reads task lifecycle state through the task status API. |
| `holon task output` | `<TASK_ID>` | `--block`; `--timeout-ms <MS>`; `--agent <AGENT>` | pretty JSON `TaskOutputResult` | `experimental` | Output preview length follows the task's creation-time `--max-output-tokens`; this command controls readiness waiting only. |
| `holon task input` | `<TASK_ID>` | required `--text <TEXT>`; `--agent <AGENT>` | pretty JSON `TaskInputResult` | `experimental` | Sends trusted operator text to command-task stdin/TTY or supervised child-agent follow-up input. |
| `holon task stop` | `<TASK_ID>` | `--agent <AGENT>` | pretty JSON `TaskStopResult` | `experimental` | Requests managed-task cancellation through the control plane. |
| `holon timer` | none | required `--after-ms <MS>`; `--every-ms <MS>`; `--summary <SUMMARY>`; `--agent <AGENT>` | pretty JSON control-plane response | `experimental` | Timer surface should be aligned with WorkItem/waiting-plane contract. |

### Agent lifecycle and model selection

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon agent` / `holon agents` | optional subcommand | none | defaults to `agent list` JSON | `stable` candidate | `agents` is an alias. |
| `holon agent list` | none | none | JSON agent entries | `stable` candidate | Public multi-agent inspection surface. |
| `holon agent status` | optional `[AGENT_ID]` | none | JSON agent status | `stable` candidate | Positional agent id; defaults to configured default agent. |
| `holon agent create` | `<AGENT_ID>` | `--template <TEMPLATE>` | pretty JSON control-plane response | `stable` candidate | Template identifier contract should align with agent initialization docs. |
| `holon agent start` | optional `[AGENT_ID]` | none | JSON lifecycle control response | `stable` candidate | Replacement for deprecated `control start`. |
| `holon agent stop` | optional `[AGENT_ID]` | none | JSON lifecycle control response | `stable` candidate | Replacement for deprecated `control stop`. |
| `holon agent abort` | optional `[AGENT_ID]` | none | pretty JSON control-plane response | `stable` candidate | Replacement for deprecated `control abort`; shares the lifecycle JSON output path with start/stop. |
| `holon agent model get` | optional `[AGENT_ID]` | none | JSON model override/status fragment | `stable` candidate | Reads `summary.model` from agent status. |
| `holon agent model set` | `<MODEL> [AGENT_ID]` | none | JSON model override response | `stable` candidate | Positional `AGENT_ID` is tested. |
| `holon agent model clear` | optional `[AGENT_ID]` | none | JSON model override response | `stable` candidate | Should share contract with set/get. |
| `holon control` | `<start\|stop\|abort>` | `--agent <AGENT>` | pretty JSON lifecycle response | `deprecated` | Use `holon agent start|stop|abort [agent-id]`. |

Deprecated `holon control` compatibility is documented in
[CLI stability policy](./cli-stability-policy.md#deprecated-holon-control).
New automation should use the `holon agent ...` lifecycle commands.

### Skills

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon skills list` | none | `--agent <AGENT>` | JSON installed-skill response | `experimental` | Skill discovery/install contract is still active design work. |
| `holon skills install` | `<NAME_OR_PATH>` | `--builtin`; `--remote`; `--skill <SKILL>`; `--copy`; `--agent <AGENT>` | pretty JSON control-plane response | `experimental` | Local paths are resolved relative to cwd when they are directories; otherwise treated as named skills. |
| `holon skills uninstall` | `<NAME>` | `--agent <AGENT>` | pretty JSON control-plane response | `experimental` | Shares the normalized JSON output path with install/list. |

### One-shot and solve workflows

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon run` | `<TEXT>` | `--trust <TRUST>` default `trusted-operator`; `--json`; `--agent <AGENT>`; `--create-agent`; `--template <TEMPLATE>`; `--max-turns <N>`; `--no-wait-for-tasks`; `--home <HOME>`; `--workspace-root <PATH>`; `--cwd <PATH>` | human `render_text()` by default; pretty JSON with `--json` | `stable` candidate for command shape; `experimental` for output | Core user entry point. JSON response shape should be locked before stable automation guidance. |
| `holon solve` | `<REF>` | `--repo <REPO>`; `--base <BASE>`; `--goal <GOAL>`; `--role <ROLE>`; `--agent <AGENT>`; `--template <TEMPLATE>`; `--model <MODEL>`; `--max-turns <N>`; `--trust <TRUST>` default `trusted-operator`; `--json`; `--home <HOME>`; `--workspace <PATH>`; `--workspace-root <PATH>`; `--cwd <PATH>`; `--input <INPUT>`; `--output <OUTPUT>` | human `render_text()` by default; pretty JSON with `--json` | `experimental` | GitHub/task workflow surface. `--workspace` and `--workspace-root` are currently coalesced. |

### Workspace

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon workspace attach` | `<PATH>` | `--agent <AGENT>` | JSON attach response | `stable` candidate | Workspace identity/projection contract is central to runtime stability. |
| `holon workspace exit` | none | `--agent <AGENT>` | JSON exit response | `stable` candidate | Should align with workspace-binding RFCs. |
| `holon workspace detach` | `<WORKSPACE_ID>` | `--agent <AGENT>` | JSON detach response | `stable` candidate | `WORKSPACE_ID` stability belongs to API/runtime inventory. |

### TUI

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon tui` | none | `--no-alt-screen`; `--connect <URL>`; `--token <TOKEN>`; `--token-file <PATH>`; `--token-profile <PROFILE>` | interactive terminal UI | `experimental` | `--connect` requires exactly one token source. TUI is not the primary stable runtime contract. |

### Debug utilities

| Command | Args | Options | Output | Initial stability | Notes |
|---|---|---|---|---:|---|
| `holon debug prompt` | `<TEXT>` | `--agent <AGENT>`; `--trust <TRUST>` default `trusted-operator` | human prompt dump | `internal` | Debug-only prompt inspection. |
| `holon debug latency` | none | `--agent <AGENT>`; `--limit <LIMIT>` default `10`; `--events-limit <EVENTS_LIMIT>` default `5000` | human latency report | `internal` | Useful diagnostics; prose should not be machine contract. |
| `holon debug scheduler-fixture` | none | `--agent <AGENT>`; required `--output <OUTPUT>` | writes JSON/JSONL fixture files; prints export summary | `internal` | Fixture file shape may be useful for tests but should be documented separately if stabilized. |

## Environment and config inputs touched by CLI

This is not the full configuration inventory; it lists environment variables
that visibly affect CLI behavior while invoking commands.

| Input | Used by | Current behavior | Initial stability |
|---|---|---|---:|
| `HOLON_HOME` | config/credentials and runtime config loading | Selects Holon home/config/credential paths. | `stable` candidate |
| `HOLON_HTTP_ADDR` | runtime/control-plane commands | Selects local control-plane HTTP address when loading `AppConfig`. | `stable` candidate |
| `HOLON_CALLBACK_BASE_URL` | `serve`, runtime config | Sets callback base URL default. | `experimental` |
| `HOLON_SOCKET_PATH` | daemon/serve | Selects local control socket path. | `experimental` |
| `HOLON_WORKSPACE_DIR` | runtime commands | Sets default workspace directory. | `experimental` |
| `HOLON_AGENT_ID` | commands with optional `--agent` / `[AGENT_ID]` | Sets default agent id. | `stable` candidate |
| `HOLON_CONTROL_TOKEN` | `serve`, daemon, control-plane client config | Supplies bearer token/control auth. | `stable` candidate |
| `HOLON_CONTROL_AUTH_MODE` | control-plane config | Parses `auto`, `required`, or `disabled`. | `experimental` |
| `HOLON_MODEL` | `run`, `solve`, provider config | Sets default model; `solve --model` writes this env var for the process. | `stable` candidate |
| Provider API-key env vars | provider-backed commands | Examples include `OPENAI_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, and configured custom env names. | `stable` candidate for documented provider envs |
| `RUST_LOG` and tracing env filter | all commands | Controls tracing output to stderr. | `internal` |

## Output contract gaps

1. **JSON responses now share the normalized stdout path for script-facing
   commands.** The remaining output work is assigning schema owners and
   stability levels to each response shape.
2. **Exit codes now have a baseline process contract.** See
   [CLI exit codes](./cli-exit-codes.md). Command-specific business-state
   promotion to non-zero exits remains intentionally opt-in and must be
   documented per command.
3. **Machine-readable outputs need schema owners.** CLI JSON often mirrors
   control-plane/runtime structs. The API inventory should decide which fields
   are stable, diagnostic, or internal.
4. **Human output is mixed with operational summaries.** `serve`, `run`,
   `solve`, and debug commands should clearly state whether their stdout is
   script-safe.
5. **Deprecated `control` remains reachable.** It should keep compatibility
   until a removal plan is documented.
6. **Help snapshots are manual.** `docs/website/reference/cli.md` says it is
   regenerated from `holon --help`, but there is no checked-in generator or
   snapshot test.

## Tracking issues

The initial CLI/API stability follow-up work is tracked in the
[CLI/API Stability Contracts](https://github.com/holon-run/holon/milestone/8)
milestone:

| Priority | Issue | Scope |
|---:|---|---|
| 0 | [#1388](https://github.com/holon-run/holon/issues/1388) | Normalize or explicitly document raw HTTP response output paths. |
| 0 | [#1389](https://github.com/holon-run/holon/issues/1389) | Define and test baseline CLI exit-code behavior. |
| 0 | [#1390](https://github.com/holon-run/holon/issues/1390) | Add normalized command tree snapshot tests. |
| 0 | [#1391](https://github.com/holon-run/holon/issues/1391) | Publish CLI stability levels and support policy. |
| 1 | [#1392](https://github.com/holon-run/holon/issues/1392) | Add task lifecycle management commands. |
| 1 | [#1393](https://github.com/holon-run/holon/issues/1393) | Add WorkItem inspection commands. |
| 1 | [#1394](https://github.com/holon-run/holon/issues/1394) | Clarify event-stream CLI surface versus `tail` and `transcript`. |
| 2 | [#1395](https://github.com/holon-run/holon/issues/1395) | Track deferred automation convenience additions. |

## Recommended next contract tests

| Priority | Test | Purpose |
|---:|---|---|
| 1 | Clap command tree/help snapshot with normalized whitespace | Detect accidental command/flag drift. |
| 1 | Parse tests for stable candidate positional args and aliases | Lock high-value CLI shape without over-snapshotting prose. |
| 1 | `--json` smoke tests for `run`/`solve` with a fake provider or fixture | Confirm machine-readable mode remains parseable. |
| 2 | Config command golden JSON for `get`, `set`, `unset`, `schema`, provider remove, credential list/remove | Lock offline scripting surfaces. |
| 2 | Daemon/status/log JSON shape tests | Lock local operations surfaces. |
| 2 | More error behavior tests for missing token and command-specific business states | Extend the baseline exit-code contract where commands promote domain failures to process failures. |
| 3 | Normalize or document raw HTTP-body commands (`task`, `timer`, `agent create/abort`, skills install/uninstall) | Reduce output contract drift. |

## Follow-up inventory scope

After this CLI inventory is reviewed, continue in this order:

1. Control-plane HTTP endpoints and response schemas used by the CLI.
2. Runtime message/event/task/work-item envelopes.
3. Public model-facing tool schemas and result envelopes.
4. Rust crate public API, if it is intended to be consumed externally.
