---
title: Create your first agent
summary: "From zero to your first Holon agent: build, start, TUI basics, create an agent, and configure models."
order: 20
---

# Create your first agent

This guide walks you through creating your first Holon agent from scratch. You will:

1. Build the Holon binary
2. Start the runtime (CLI mode and daemon mode)
3. Connect with the TUI
4. Create an agent
5. Configure models

**Time:** ~15 minutes

## Prerequisites

- **Rust toolchain** with Cargo (for building from source)
- **A model provider** account (Anthropic, OpenAI, or compatible)
- **Basic terminal** familiarity

## Step 1: Build Holon

### Clone and build

```bash
git clone https://github.com/holon-run/holon.git
cd holon
cargo build
```

### Verify the build

```bash
cargo run -- --help
```

You should see the CLI help output with available commands.

## Step 2: Start the Holon runtime

Holon can run in two modes:

- **CLI mode** (default): Single-shot commands with direct output
- **Daemon mode**: Background service with TUI support

### Option A: CLI mode (quick start)

Run a single command:

```bash
cargo run -- run "What is Holon?"
```

This executes one turn and exits. Great for quick tasks, but not ideal for interactive sessions.

### Option B: Daemon mode (recommended for agents)

Start the background daemon:

```bash
cargo run -- daemon start
```

This starts Holon as a background service with:

- **Unix socket** at `~/.holon/run/holon.sock` (local access)
- **Control plane** for TUI and HTTP clients
- **Persistent state** across sessions

#### Verify the daemon is running

```bash
cargo run -- daemon status
```

You should see runtime status including the default agent.

#### Stop the daemon

```bash
cargo run -- daemon stop
```

## Step 3: Connect with the TUI

The **Terminal UI (TUI)** provides an interactive interface for working with agents.

### Start the TUI

```bash
cargo run -- tui
```

The TUI connects to the local Unix socket by default.

### TUI basics

The TUI shows:

- **Agent list**: Current agents and their status
- **Active agent**: The agent receiving your input
- **Transcript**: Conversation history
- **Task list**: Background tasks and work items

### Basic navigation

- **Type** your message and press Enter to send
- **Ctrl+C** to exit the TUI
- **Arrow keys** or **Page Up/Down** to scroll through history

### Remote TUI (optional)

To connect to a remote Holon instance:

```bash
# On the remote host
cargo run -- serve --access lan --host 192.168.1.10 --token-file ~/.holon/remote.token

# From your local machine
cargo run -- tui --connect http://192.168.1.10:7878 --token-file ~/.holon/remote.token
```

See [Remote TUI Access RFC](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-tui-access.md) for details.

## Step 4: Create an agent

Holon supports different agent types for different use cases.

### The default agent

When you start Holon, it automatically creates a **default agent** in `~/.holon/agents/main/`. This agent:

- Has its own `agent_home` with `AGENTS.md`
- Can have agent-local skills
- Stores conversation history and work state

### Create a named agent

Create a specialized agent for a specific role:

```bash
cargo run -- agent create reviewer --template holon-reviewer
```

This creates a new agent in `~/.holon/agents/reviewer/` initialized with the holon-reviewer template.

### Use a template

Templates provide reusable agent configurations:

```bash
# Use a builtin template by ID
cargo run -- agent create docs-helper --template holon-developer

# Use a local template path
cargo run -- agent create custom --template /path/to/template

# Use a GitHub template URL
cargo run -- agent create github-agent --template https://github.com/owner/repo/tree/main/template-path
```

### List agents

```bash
cargo run -- agent list
```

### Switch agents in the TUI

In the TUI, you can switch between agents using the agent list view.

## Step 5: Configure models

Holon requires model configuration to work with providers like Anthropic or OpenAI.

### Configuration layers

Holon uses three configuration layers:

1. **Startup settings** (environment variables, CLI flags)
2. **Runtime configuration** (`config.json`)
3. **Agent state** (per-agent overrides)

See [Runtime Configuration Surface RFC](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-configuration-surface.md) for details.

### Set provider credentials

#### Option A: Environment variables (recommended for startup)

```bash
# Anthropic
export ANTHROPIC_AUTH_TOKEN="your-api-key"

# OpenAI
export OPENAI_API_KEY="your-api-key"

# Then start Holon
cargo run -- daemon start
```

#### Option B: config.json (for runtime changes)

Create or edit `~/.holon/config.json`:

```json
{
  "model": {
    "default": "anthropic/claude-sonnet-4-6"
  },
  "providers": {
    "anthropic": {
    }
  }
}
```

### Set the default model

```bash
# Via CLI
cargo run -- config set model.default "anthropic/claude-sonnet-4-6"

# Or edit config.json directly
```

### Agent-level model override

An agent can override the default model:

```bash
cargo run -- agent model set "anthropic/claude-sonnet-4-6" reviewer
```

See [Configuration reference](/reference/configuration.md) for more details on agent model overrides.

> **Note**: For security, Holon uses a credential store to manage API keys. Set credentials via:
> ```bash
> cargo run -- config credentials set --kind api_key --stdin anthropic
> # Paste your ANTHROPIC_AUTH_TOKEN and press Enter
> ```
> See [Configuration reference](/reference/configuration.md) for details.

### Verify model configuration
```bash
cargo run -- config get model.default
cargo run -- config list
```

To see available models:

```bash
cargo run -- config models list
```

## Next steps

Now that you have your first agent running:

- **Explore concepts**: Read [Runtime model](/concepts/runtime-model.md) and [Trust boundaries](/concepts/trust-boundaries.md)
- **Try examples**: See [Quick examples](/guides/quick-examples.md) for common tasks
- **Build integrations**: Check the [Integration guide](/guides/integration.md)
- **Reference documentation**: See [CLI reference](/reference/cli.md), [HTTP control plane](/reference/http-control-plane.md), and [Configuration reference](/reference/configuration.md)

## Troubleshooting

### Daemon won't start

Check if another instance is running:

```bash
cargo run -- daemon status
cargo run -- daemon stop
cargo run -- daemon start
```

### TUI can't connect

Verify the daemon is running and the socket exists:

```bash
ls ~/.holon/run/holon.sock
cargo run -- daemon status
```

### Model errors

Verify your credentials:

```bash
echo $ANTHROPIC_AUTH_TOKEN
cargo run -- config get model.default
```

For more help, see [Troubleshooting guide](/guides/troubleshooting.md).
