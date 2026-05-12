---
title: CLI reference
summary: Holon's current command-line interface as compiled — verified against `holon --help`.
order: 10
---
<!-- maintenance: regenerate from `holon --help` output when commands change -->

# CLI Reference

Holon's command-line interface. All commands accept `--help` for detailed flag documentation.

## Command Tree

```
holon (v0.13.0)
├── run          One-shot agent interaction
├── prompt       Send a prompt to an agent (lightweight)
├── status       Show agent status
├── tail         Show recent log tail
├── transcript   Show conversation transcript
├── task         Run a command as a background task
├── timer        Create a delayed or recurring timer
├── control      [deprecated] use `holon agent pause|resume|stop|abort`
├── daemon       Background daemon lifecycle
│   ├── start    Start the daemon
│   ├── stop     Stop the daemon
│   ├── status   Check daemon status
│   ├── restart  Restart the daemon
│   └── logs     View daemon logs
├── agent        Agent management
│   ├── list     List all agents
│   ├── status   Show agent status
│   ├── create   Create a new agent
│   ├── pause    Pause an agent
│   ├── resume   Resume an agent
│   ├── stop     Stop an agent
│   ├── abort Abort current run
│   └── model    Per-agent model configuration
│       ├── get  Get agent model override
│       ├── set  Set agent model override
│       └── clear Clear agent model override
├── serve        Start HTTP control plane server
├── skills       Manage skills
│   ├── list     List installed skills
│   ├── install  Install a skill
│   └── uninstall Uninstall a skill
├── tui          Launch interactive terminal UI
├── config       Runtime configuration
│   ├── get      Read a config key
│   ├── set      Write a config key
│   ├── unset    Remove a config key
│   ├── list     List all current config
│   ├── schema   Show all config keys with types and defaults
│   ├── doctor   Full system health check
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
│   └── models  Model catalog
│       └── list List available models
├── solve        Solve a GitHub issue or similar target
├── workspace    Workspace management (attach, exit, detach)
├── debug        Debug utilities
│   ├── prompt   Debug-mode prompt
│   └── latency  Show latency metrics
└── help         Print help
```

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
holon agent pause reviewer
holon agent resume reviewer
holon agent stop reviewer
holon agent abort reviewer
```

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

## See Also

- [Configuration Reference](/reference/configuration.md) — Config keys and credential management
- [HTTP Control Plane](/reference/http-control-plane.md) — HTTP API design philosophy
- [Getting Started](/getting-started/first-agent.md) — Setup tutorial
- [Quick Examples](/guides/quick-examples.md) — Task-oriented examples
