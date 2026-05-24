---
title: CLI reference
summary: Holon's current command-line interface — verified against holon --help (v0.14.0).
order: 10
---
<!-- maintenance: regenerate from `holon --help` output when commands change -->

# CLI Reference

Holon's command-line interface. All commands accept `--help` for detailed flag documentation.

For scripting guidance, stability levels, and support policy, see
[CLI stability policy](./cli-stability-policy.md) and
[CLI contract inventory](./cli-contract-inventory.md).

## Command Tree

```
holon (v0.14.0)
├── serve        Start HTTP control plane server
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
├── status       Show agent status
├── tail         Show recent log tail
├── transcript   Show conversation transcript
├── task         Run a command as a background task
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
├── skills       Manage skills
│   ├── list     List installed skills
│   ├── install  Install a skill
│   └── uninstall Uninstall a skill
├── run          One-shot agent interaction
├── solve        Solve a GitHub issue or similar target
├── workspace    Workspace management (attach, exit, detach)
│   ├── attach   Attach to an existing workspace
│   ├── exit     Exit current workspace
│   └── detach   Detach from a workspace
├── tui          Launch interactive terminal UI
├── debug        Debug utilities
│   ├── prompt   Debug-mode prompt
│   ├── latency  Show latency metrics
│   └── scheduler-fixture Generate scheduler fixture data
└── help         Print help
```

> **Note:** This reference reflects the latest release. If you are running a
> source build from `main`, some commands or flags may differ. Always run
> `holon --help` and `holon <COMMAND> --help` for the live command reference
> of your installed version.

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
holon config set model.default "deepseek-anthropic/deepseek-v4-pro"
holon agent model set "anthropic/claude-sonnet-4-6" reviewer
holon agent model get reviewer
holon agent model clear reviewer
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

### Configuration inspection

```bash
holon config list                # All current config
holon config schema              # All keys with types and defaults
holon config doctor              # Full health check
holon config providers list      # All registered providers
holon config models list         # Available models with status
holon config credentials list    # Stored credential profiles
```

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
