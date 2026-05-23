---
title: Create your first agent
summary: "From zero to your first Holon agent: install, start, TUI basics, create an agent, and configure models."
order: 20
updated: 2026-05-23
---

# Create your first agent

This guide walks you through creating your first Holon agent from scratch. You will:

1. Install Holon
2. Start the runtime (one-shot and daemon mode)
3. Connect with the TUI
4. Create an agent
5. Configure models

**Time:** ~10 minutes (with Homebrew); ~15 minutes (build from source)

## Prerequisites

- **Holon** installed on `PATH` (see Step 1)
- **A model provider** account (Anthropic, OpenAI, or compatible)
- **Basic terminal** familiarity

## Step 1: Install Holon

Holon v0.14.0 and later ship as installable releases. Choose the method that
works best for you.

### Option A: Homebrew (recommended)

```bash
brew tap holon-run/tap
brew install holon
```

### Option B: Direct binary download

Download the latest binary for your platform from the
[GitHub Releases page](https://github.com/holon-run/holon/releases/latest),
then place it on your `PATH`:

```bash
# Example: Linux amd64
curl -L https://github.com/holon-run/holon/releases/latest/download/holon-linux-amd64 -o holon
chmod +x holon
sudo mv holon /usr/local/bin/
```

Binaries are available for Linux amd64, macOS amd64, and macOS arm64.

### Option C: Build from source

If you prefer to build from source or plan to contribute to Holon:

```bash
git clone https://github.com/holon-run/holon.git
cd holon
cargo build --release
```

When building from source, replace `holon` commands in the examples below with
`cargo run --`. For example, `holon --help` becomes `cargo run -- --help`.

### Verify the install

```bash
holon --help
```

You should see the CLI help output with available commands.

## Step 2: Start the Holon runtime

Holon can run in two modes:

- **CLI mode** (default): Single-shot commands with direct output
- **Daemon mode**: Background service with TUI support

### Option A: CLI mode (quick start)

Run a single command:

```bash
holon run "What is Holon?"
```

This executes one turn and exits. Great for quick tasks, but not ideal for interactive sessions.

### Option B: Daemon mode (recommended for agents)

Start the background daemon:

```bash
holon daemon start
```

This starts Holon as a background service with:

- **Unix socket** at `~/.holon/run/holon.sock` (local access)
- **Control plane** for TUI and HTTP clients
- **Persistent state** across sessions

#### Verify the daemon is running

```bash
holon daemon status
```

You should see runtime status including the default agent.

#### Stop the daemon

```bash
holon daemon stop
```

## Step 3: Connect with the TUI

The **Terminal UI (TUI)** provides an interactive interface for working with agents.

### Start the TUI

```bash
holon tui
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
holon serve --access lan --host 192.168.1.10 --token-file ~/.holon/remote.token

# From your local machine
holon tui --connect http://192.168.1.10:7878 --token-file ~/.holon/remote.token
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
holon agent create reviewer --template holon-reviewer
```

This creates a new agent in `~/.holon/agents/reviewer/` initialized with the holon-reviewer template.

### Use a template

Templates provide reusable agent configurations:

```bash
# Use a builtin template by ID
holon agent create docs-helper --template holon-developer

# Use a local template path
holon agent create custom --template /path/to/template

# Use a GitHub template URL
holon agent create github-agent --template https://github.com/owner/repo/tree/main/template-path
```

### List agents

```bash
holon agent list
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

#### Option A: Environment variables (quick start)

```bash
# Anthropic
export ANTHROPIC_AUTH_TOKEN="your-api-key"

# OpenAI
export OPENAI_API_KEY="your-api-key"
```

#### Option B: Credential store (recommended for persistent config)

Store credentials securely without exposing them in shell history or
environment variables:

```bash
holon config credentials set --kind api_key --stdin anthropic
# Paste your ANTHROPIC_AUTH_TOKEN and press Enter
```

### Set the default model

```bash
holon config set model.default "anthropic/claude-sonnet-4-6"
```

### Agent-level model override

An agent can override the default model:

```bash
holon agent model set "anthropic/claude-sonnet-4-6" reviewer
```

See [Configuration reference](/reference/configuration.md) for more details on agent model overrides.

### Verify model configuration

```bash
holon config get model.default
holon config doctor
```

To see available models:

```bash
holon config models list
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
holon daemon status
holon daemon stop
holon daemon start
```

### TUI can't connect

Verify the daemon is running and the socket exists:

```bash
ls ~/.holon/run/holon.sock
holon daemon status
```

### Model errors

Verify your credentials:

```bash
echo $ANTHROPIC_AUTH_TOKEN
holon config get model.default
```

For more help, see [Troubleshooting guide](/guides/troubleshooting.md).
