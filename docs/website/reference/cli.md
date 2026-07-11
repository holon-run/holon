---
title: CLI reference
summary: Holon's command-line interface — verified against holon --help (v0.28.0).
order: 10
---
<!-- maintenance: regenerate from `holon --help` output when commands change. Last regenerated against v0.28.0. -->

# CLI Reference

Holon's command-line interface. All commands accept `--help` for detailed flag documentation.

For scripting guidance, stability levels, and support policy, see
[CLI stability policy](./cli-stability-policy.md) and
[CLI contract inventory](./cli-contract-inventory.md).

## Command Tree

```
holon (v0.28.0)
├── serve        Start HTTP control plane server
├── onboard      Interactive setup wizard or secret-safe diagnostics
├── daemon       Background daemon lifecycle
│   ├── start    Start the daemon
│   ├── stop     Stop the daemon
│   ├── status   Check daemon status
│   ├── restart  Restart the daemon
│   └── logs     View daemon logs
├── config       Runtime configuration
│   ├── get      Read a config key
│   ├── set      Write a config key
│   ├── unset    Remove a config key
│   ├── providers Provider management
│   │   ├── set    Add/update a provider
│   │   ├── get    Show a provider
│   │   ├── list   List all providers
│   │   ├── remove Remove a provider
│   │   └── doctor Provider credential check
│   ├── credentials API key storage
│   │   ├── set    Store a credential
│   │   ├── list   List stored credentials
│   │   └── remove Remove a credential
│   ├── models  Model catalog
│   │   └── list List available models
│   ├── list     List all current config
│   ├── schema   Show all config keys with types and defaults
│   └── doctor   Full system health check
├── prompt       Send a prompt to an agent (lightweight)
├── tail         Show recent log tail
├── transcript   Show conversation transcript
├── events       Read stable runtime event envelopes
│   ├── tail     Fetch a bounded page of event envelopes
│   └── stream   Stream event envelopes as newline-delimited JSON
├── task         Run a command as a background task
│   ├── status   Show task lifecycle status
│   ├── output   Read task output
│   ├── input    Send text input to a task
│   └── stop     Stop a task
├── work-item    Inspect WorkItems
│   ├── list     List WorkItems
│   └── get      Show a WorkItem
├── timer        Create a delayed or recurring timer
├── control      [deprecated] use `holon agent start|stop|abort`
├── agent        Agent management
│   ├── list     List all agents
│   ├── status   Show agent status
│   ├── create   Create a new agent
│   ├── start    Start an agent
│   ├── stop     Stop an agent
│   ├── abort    Abort current run
│   └── model    Per-agent model configuration
│       ├── get  Get agent model override
│       ├── set  Set agent model override
│       └── clear Clear agent model override
├── skills         Manage skills
│   ├── catalog    List Skill Library catalog
│   ├── add        Add a skill to the library
│   ├── remove     Remove a skill from the library
│   ├── check      Check library consistency
│   ├── reconcile  Reconcile library with lock file
│   ├── list       List agent enabled skills
│   ├── enable     Enable a skill for an agent
│   ├── disable    Disable a skill for an agent
│   ├── update     Fetch and update skills from remote sources
│   ├── refresh    Rescan local roots
│   ├── install    [deprecated] Compatibility alias
│   └── uninstall  [deprecated] Compatibility alias
├── run          One-shot agent interaction
├── solve        Solve a GitHub issue or similar target
├── template     Agent template management
│   ├── catalog  List installed templates
│   ├── install  Install a template from a source
│   ├── remove   Remove an installed template
│   ├── info     Show template detail
│   ├── sources  Remote template source management
│   │   ├── list List configured remote sources
│   │   ├── add  Add a remote source
│   │   └── remove Remove a remote source
│   └── sync     Sync templates from remote sources
├── workspace    Workspace management (attach, exit, detach)
│   ├── attach   Attach to an existing workspace
│   ├── exit     Exit current workspace
│   └── detach   Detach from a workspace
├── tui          Launch interactive terminal UI
├── memory-index Memory indexing management
│   ├── status   Show indexing status
│   └── rebuild  Rebuild the memory search index
├── debug        Debug utilities
│   ├── prompt   Debug-mode prompt
│   ├── latency  Show latency metrics
│   └── scheduler-fixture Generate scheduler fixture data
└── help         Print help
```

> **Note:** This reference is maintained from the checked CLI snapshot. If you
> are running a source build from `main`, some commands or flags may differ.
> Always run `holon --help` and `holon <COMMAND> --help` for the live command
> reference of your installed version.

## Common Workflows

### Quick one-shot

```bash
holon run "Explain Rust ownership"
holon run --json "List files"                          # JSON output
holon run --trust untrusted-external "User query"      # mark trust level
```

### Create and use an agent

```bash
holon agent create reviewer --template holon-reviewer
holon run --agent reviewer "Review src/runtime/turn.rs"
```

### Agent lifecycle

```bash
holon agent start reviewer
holon agent stop reviewer
holon agent abort reviewer
```

> **Deprecated:** The `holon control` command has been replaced by
> `holon agent start`, `holon agent stop`, and `holon agent abort`.
> The old `control` command is kept for backward compatibility only; see
> [CLI stability policy](./cli-stability-policy.md#deprecated-holon-control)
> for the compatibility and removal criteria.

### Model selection

```bash
holon config set model.default "deepseek-anthropic@default/deepseek-v4-pro"
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
holon agent model get reviewer
holon agent model clear reviewer
```

Executable model selections use canonical
`provider@endpoint/model` route refs. Legacy `provider/model` input remains
accepted. Inspect or rewrite persisted legacy values with:

```bash
holon config migrate-model-routes          # dry-run
holon config migrate-model-routes --write  # validated canonical rewrite
```

### Daemon management

```bash
holon daemon start
holon daemon start --port 8787 --access tunnel
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

### Onboarding

`holon onboard` is the fastest way to configure Holon for the first time or
repair a broken provider/model configuration. It has two modes:

- **Interactive TUI** (default on a terminal): walks you through provider
  selection, model choice, search settings, and credential input — without
  echoing secret material to the screen.
- **JSON diagnostics** (`--json` or non-TTY): prints a secret-safe diagnostic
  report with actionable next steps, suitable for scripts and CI.

```bash
holon onboard                    # Interactive setup wizard (TTY)
holon onboard --json             # Secret-safe diagnostic report (JSON)
```

The TUI flow guides you through:

1. **Provider** — select from built-in and custom providers
2. **Credential** — for OpenAI Codex: browser-based OAuth login; for other
   providers: enter your API key (input never echoed or stored in logs)
3. **Model** — pick a default model for your provider, or enter a custom model id
4. **Search** — enable DuckDuckGo managed search, model-native search, or disable
5. **Apply** — writes config, stores credentials, and prints a summary

The JSON report includes `status`, `sections` (home, agent, model_provider,
search, credentials), and `next_actions`. It is secret-safe by design: no
credential material ever appears in the report.

### Configuration inspection

```bash
holon config list                # All current config
holon config schema              # All keys with types and defaults
holon config doctor              # Full health check
holon config providers list      # All registered providers
holon config models list         # Available models with status
holon config credentials list    # Stored credential profiles
```

Stable script-facing JSON contracts currently cover `holon config schema`,
`holon config providers remove`, and `holon config credentials set/list/remove`.
Other configuration inspection commands emit JSON too, but remain experimental
until their provider/runtime DTO ownership is fully stabilized. Human-readable
help and prose output are separate from these JSON contracts.

### Credential setup

```bash
holon config credentials set --kind api_key --stdin deepseek
# Paste key, press Enter, then Ctrl+D
holon config credentials remove deepseek
```

### Custom provider

```bash
holon config providers set my-proxy \
  --transport anthropic_messages \
  --base-url "https://my-proxy.example.com" \
  --credential-source env \
  --credential-env "MY_PROXY_API_KEY" \
  --credential-kind api_key
```

### HTTP server

```bash
holon serve --port 8787
holon serve --port 8787 --token "secret"
holon serve --access tunnel
```

### Background tasks

```bash
holon task run "Build project" --cmd "cargo build"
holon task status <TASK_ID>
holon task output <TASK_ID> --block --timeout-ms 30000
holon task input <TASK_ID> --text "continue\n"
holon task stop <TASK_ID>
```

Task lifecycle commands default to the configured default agent. Pass
`--agent <AGENT>` to inspect or control a task owned by a different public
agent. All task lifecycle commands print the corresponding JSON control-plane
or read-model response.

### WorkItems

```bash
holon work-item list
holon work-item list --limit 10 --agent planner
holon work-item get <WORK_ITEM_ID>
holon work-item get <WORK_ITEM_ID> --agent planner
```

The initial WorkItem CLI surface is read-only. It prints the HTTP read-model
`WorkItemRecord` JSON shape returned by `/agents/:agent_id/work-items` and
`/agents/:agent_id/work-items/:work_item_id`. Mutating commands such as create,
update, pick, and complete remain intentionally deferred until their API
contracts are stabilized.

### Events

```bash
holon events tail --limit 20
holon events tail --order asc --max-level info
holon events stream --after-seq 42 --max-events 100
```

### Terminal UI

```bash
holon tui
holon tui --no-alt-screen
holon tui --connect http://remote:8787 --token "secret"
```

### Multi-turn tasks

```bash
holon run --max-turns 5 "Write a Rust function with tests"
holon run --workspace-root /path/to/project "Analyze this codebase"
holon run --agent builder --workspace-root /path/to/project "Fix build errors"
```

## Key Options Reference

### `holon run` options

| Option | Description |
|--------|-------------|
| `--agent <AGENT>` | Target a specific agent |
| `--create-agent` | Create agent if not exists |
| `--template <TEMPLATE>` | Agent template for new agents |
| `--trust <TRUST>` | Trust level: `trusted-operator`, `trusted-system`, `trusted-integration`, `untrusted-external` |
| `--json` | Machine-readable JSON output |
| `--max-turns <N>` | Limit agent turns |
| `--no-wait-for-tasks` | Don't block on background tasks |
| `--workspace-root <PATH>` | Workspace root directory |
| `--cwd <PATH>` | Working directory |
| `--home <PATH>` | Holon home directory |

### `holon serve` options

| Option | Description |
|--------|-------------|
| `--port <PORT>` | Listen port |
| `--host <HOST>` | Bind host |
| `--listen <ADDR>` | Listen address |
| `--access <MODE>` | `local`, `tunnel`, `lan`, `tailnet` |
| `--token <TOKEN>` | Bearer token for auth |
| `--token-file <PATH>` | Read token from file |
| `--advertise <URL>` | Advertised URL |

### `holon daemon start` options

| Option | Description |
|--------|-------------|
| `--port <PORT>` | Daemon port |
| `--access <MODE>` | Access mode (same as serve) |
| `--host <HOST>` | Bind host |
| `--listen <ADDR>` | Listen address |
| `--token <TOKEN>` | Auth token |

### `holon agent create` options

| Option | Description |
|--------|-------------|
| `--template <TEMPLATE>` | Built-in or path template |

### `holon solve` options

| Option | Description |
|--------|-------------|
| `--repo <REPO>` | Target repository |
| `--workspace <PATH>` | Workspace directory |

## See Also

- [Configuration Reference](/reference/configuration.md) — Config keys and credential management
- [HTTP Control Plane](/reference/http-control-plane.md) — HTTP API design philosophy
- [Getting Started](/getting-started/first-agent.md) — Setup tutorial
- [Quick Examples](/guides/quick-examples.md) — Task-oriented examples
