---
title: Quick examples
summary: Common Holon tasks you can try after completing the Getting Started guide.
order: 15
---

# Quick Examples

Common Holon tasks you can try after completing the [Getting Started guide](/getting-started/first-agent.md).

## 1. One-Shot Question

Ask a question without creating a persistent agent:

```bash
holon run "Explain how Rust ownership works in three sentences"
```

Use `--json` for machine-readable output:

```bash
holon run --json "List files in the current directory"
```

## 2. Create an Agent

Create a named agent that persists across sessions:

```bash
holon agent create reviewer
```

Create an agent from a built-in template:

```bash
holon agent create reviewer --template holon-reviewer
```

Then interact with it:

```bash
holon run --agent reviewer "Review the changes in src/runtime/turn.rs"
```

## 3. Switch Models

Change the global default model:

```bash
holon config set model.default "deepseek-anthropic/deepseek-v4-pro"
```

Set a per-agent model override:

```bash
holon agent model set "anthropic/claude-sonnet-4-6" reviewer
```

Check which model an agent uses:

```bash
holon agent model get reviewer
```

## 4. Run as a Background Daemon

Start the daemon:

```bash
holon daemon start
```

Check daemon status:

```bash
holon daemon status
```

View daemon logs:

```bash
holon daemon logs
```

Stop the daemon:

```bash
holon daemon stop
```

Start with a specific access mode:

```bash
holon daemon start --access tunnel
```

## 5. Use the Terminal UI (TUI)

Launch the interactive terminal UI:

```bash
holon tui
```

Connect to a remote daemon via TUI:

```bash
holon tui --connect https://your-server:8787
```

## 6. Start the HTTP Server

Expose the control plane as an HTTP API:

```bash
holon serve --port 8787
```

With access control:

```bash
holon serve --port 8787 --token "your-secret-token"
```

## 7. Set Up API Credentials

Store an API key securely (preferred method):

```bash
holon config credentials set --kind api_key --stdin deepseek
# Paste your API key and press Enter, then Ctrl+D
```

Or use an environment variable:

```bash
export DEEPSEEK_API_KEY="sk-..."
holon run "Hello"
```

Verify credentials are working:

```bash
holon config doctor
```

## 8. Inspect Configuration

View all current configuration:

```bash
holon config list
```

See all available configuration keys with defaults:

```bash
holon config schema
```

List configured providers:

```bash
holon config providers list
```

List available models:

```bash
holon config models list
```

## 9. Run a Multi-Turn Task

Limit the maximum number of turns:

```bash
holon run --max-turns 5 "Write a Rust function that reverses a string, with tests"
```

Run in a specific workspace:

```bash
holon run --workspace-root /path/to/project "Analyze this codebase"
```

## 10. Create an Agent with a Custom Workspace

```bash
holon agent create my-builder
holon run --agent my-builder --workspace-root /path/to/project "Build the project"
```

## See Also

- [Getting Started](/getting-started/first-agent.md) — Full step-by-step tutorial
- [Configuration Reference](/reference/configuration.md) — All config keys and credential management
- [CLI Reference](/reference/cli.md) — Complete command-line reference
- [Troubleshooting](/guides/troubleshooting.md) — Common issues and solutions
- [Integration Guide](/guides/integration.md) — HTTP control plane integration
