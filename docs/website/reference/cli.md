---
title: CLI reference
summary: Holon's command-line interface — verified against holon --help (v0.40.0).
order: 10
---
<!-- maintenance: regenerate from `holon --help` output when commands change. Last regenerated against v0.40.0. -->

# CLI Reference

Holon's command-line interface. All commands accept `--help` for detailed flag documentation.

For scripting guidance, stability levels, and support policy, see
[CLI stability policy](./cli-stability-policy.md) and
[CLI contract inventory](./cli-contract-inventory.md).

## Command Tree

```
holon (v0.40.0)
├── context      Show the declared caller context
├── commands     Show machine-readable CLI command metadata
├── serve        Start HTTP control plane server
├── onboard      Interactive setup wizard or secret-safe diagnostics
├── daemon       Background daemon lifecycle
│   ├── start    Start the daemon
│   ├── prepare-update Stop the daemon without altering desired auto-start state
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
│   ├── models  Model catalog and discovery
│   │   ├── list    List available models
│   │   └── refresh Refresh discovered models for a provider
│   ├── migrate-model-routes  Inspect/rewrite legacy model selections
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
│   ├── list     List tasks
│   ├── run      Run a command as a managed background task
│   ├── status   Show task lifecycle status
│   ├── output   Read task output
│   ├── input    Send text input to a task
│   └── stop     Stop a task
├── work-item    Inspect and manage WorkItems
│   ├── list     List WorkItems
│   ├── get      Show a WorkItem
│   ├── create   Create a WorkItem
│   ├── pick     Pick a WorkItem as current focus
│   ├── update   Update a WorkItem
│   └── complete Complete a WorkItem
├── timer        Create, list, or cancel timers
│   ├── create   Create a delayed or recurring timer
│   ├── list     List active timers
│   └── cancel   Cancel an active timer
├── control      [deprecated] use `holon agent start|stop|abort`
├── agent        Agent management
│   ├── list     List all agents
│   ├── get      Show canonical agent detail
│   ├── status   Show agent status
│   ├── create   Create a new agent
│   ├── rename   Rename a public self-owned agent
│   ├── repair   Retry incomplete post-create bootstrap steps
│   ├── start    Start an agent
│   ├── stop     Stop an agent
│   ├── delete   Permanently delete an agent and its data
│   ├── abort    Abort current run
│   ├── reset-callback Reset the external trigger callback for an agent
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
├── workspace    Workspace management (attach, exit, detach)
│   ├── attach   Attach to an existing workspace
│   ├── exit     Exit current workspace
│   └── detach   Detach from a workspace
├── tui          Launch interactive terminal UI
├── memory-index Memory indexing management
│   └── rebuild  Rebuild the memory search index
├── models-dev   models.dev snapshot refresh, validation, and audit
│   ├── refresh  Fetch the snapshot and regenerate the artifact
│   ├── validate Validate the checked-in snapshot and artifact
│   └── audit    Audit provider mappings against a snapshot
├── debug        Debug utilities
│   ├── prompt   Debug-mode prompt
│   ├── latency  Show latency metrics
│   ├── performance  Show performance metrics
│   ├── trace    Show end-to-end trace by id or search
│   ├── runtime-db   Runtime database audit, retention, and maintenance
│   │   ├── agent-relations Report or backfill canonical agent relation records
│   │   ├── audit    Audit runtime database invariants
│   │   ├── retention Run retention cleanup on old database records
│   │   ├── compact  Compact the runtime database
│   │   ├── turn-settlement Audit or apply a fingerprint-fenced historical Turn settlement repair
│   │   └── conversation-input-assignment-rollback Preflight or rollback v66 repair marker
│   ├── scheduler-recovery  Inspect/apply scheduler recovery
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
holon run --authority-class external-evidence "User query"  # set authority class
```

### Create and use an agent

```bash
holon agent create reviewer --template code-reviewer
holon agent repair reviewer
holon run --agent reviewer "Review src/runtime/turn.rs"
```

### Agent lifecycle

```bash
holon agent start reviewer
holon agent stop reviewer
holon agent abort reviewer
holon agent delete reviewer --yes
```

> **Deprecated:** The `holon control` command has been replaced by
> `holon agent start`, `holon agent stop`, and `holon agent abort`.
> The old `control` command is kept for backward compatibility only; see
> [CLI stability policy](./cli-stability-policy.md#deprecated-holon-control)
> for the compatibility and removal criteria.

`holon agent delete` permanently removes an agent and its associated data.
Pass `--cascade-private-children` to also remove its private child agents and
`--wait` to block until the deletion job completes. It requires `--yes` in
non-interactive mode.

`holon agent repair <AGENT_ID>` retries incomplete post-create template,
runtime, workspace, model, and initial-message steps. It does not recreate the
Agent or overwrite conflicting user-managed state.

`holon agent rename <AGENT_ID> --name <NAME>` updates the display name of a
public self-owned agent and echoes the updated agent detail. The agent id is
permanent; the configured default agent cannot be renamed, and duplicate names
are rejected with a readable conflict error.

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

### models.dev provider mapping

Holon ships a checked-in [models.dev](https://models.dev) snapshot and a
versioned provider mapping manifest that reconciles upstream model metadata
with Holon provider/route identities. The `holon models-dev` subcommands
audit, validate, and refresh this snapshot:

```bash
holon models-dev validate        # validate the checked-in snapshot and artifact
holon models-dev audit           # audit provider mappings against the snapshot
holon models-dev audit --json    # machine-readable mapping audit report
holon models-dev refresh         # fetch upstream and regenerate the artifact
```

`refresh` and `validate` target the repository's checked-in `models.dev/`
files and are intended for Holon development and release automation. See
[Supported Models](./models.md) for the runtime model catalog.

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
holon work-item create "Triage failing CI"
holon work-item pick <WORK_ITEM_ID> --reason "unblock release"
holon work-item complete <WORK_ITEM_ID>
```

`list` and `get` are read-only and print the HTTP read-model `WorkItemRecord`
JSON shape returned by `/agents/:agent_id/work-items` and
`/agents/:agent_id/work-items/:work_item_id`. The `create`, `update`, `pick`,
and `complete` subcommands mutate WorkItem state and return the corresponding
control-plane response.

### Timers

```bash
holon timer create --after-ms 60000 --summary "Heartbeat check"
holon timer list
holon timer cancel <TIMER_ID>
```

`holon timer` schedules delayed or recurring timers for an agent (defaults to
the default agent, or pass `--agent <AGENT>`).

### Events

```bash
holon events tail --limit 20
holon events tail --order asc --max-level info
holon events tail --agent benchmark-run --order asc --offline
holon events stream --after-seq 42 --max-events 100
```

`events tail --offline` reads the same stable event envelope from the local
runtime database without requiring a running daemon. Offline pages do not
support `--max-level`.

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
| `--authority-class <CLASS>` | Authority class: `operator-instruction`, `runtime-instruction`, `integration-signal`, `external-evidence` (alias: `--trust`) |
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
