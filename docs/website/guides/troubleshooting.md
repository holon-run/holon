---
title: Troubleshooting
summary: Solutions for common Holon issues covering daemon, configuration, model, and TUI problems.
order: 30
---

# Troubleshooting

Solutions for common Holon issues.

## Daemon Issues

### Daemon won't start

**Symptom:** `holon daemon start` hangs or exits with an error.

**Checklist:**

```bash
# 1. Check if another instance is already running
holon daemon status

# 2. View recent daemon logs
holon daemon logs

# 3. Restart the daemon
holon daemon restart
```

If the daemon fails to bind to a port, try a different port:

```bash
holon daemon start --port 8788
```

### Daemon status shows "stopped" after starting

Check the daemon logs for crash messages:

```bash
holon daemon logs
```

Common causes:
- Port already in use by another process
- Missing or invalid credentials for the default model
- Insufficient disk space or permissions on `~/.holon/`

## Credential & Model Issues

### "No available model" or credential errors

Run the diagnostic tool:

```bash
holon config doctor
```

This shows which models are available, which credentials are configured, and detailed availability status for each provider/model pair.

### API key not recognized

1. Verify the credential is stored:
   ```bash
   holon config credentials list
   ```

2. For environment variable auth, check the variable is set:
   ```bash
   echo $DEEPSEEK_API_KEY
   ```

3. Verify the provider is registered and the credential profile matches:
   ```bash
   holon config providers list
   ```

4. If using a custom provider, check that `--credential-profile` matches the profile used in `config credentials set`:
   ```bash
   holon config providers get my-provider
   ```

### Model returns errors or unexpected behavior

1. Verify the model is available:
   ```bash
   holon config models list
   ```

2. Check the current default model:
   ```bash
   holon config get model.default
   ```

3. Try switching to a different model:
   ```bash
   holon config set model.default "anthropic/claude-sonnet-4-6"
   ```

## Agent Issues

### Agent not responding

1. Check agent exists:
   ```bash
   holon agent model get my-agent
   ```

2. Try running with a fresh one-shot:
   ```bash
   holon run "test" --agent my-agent --max-turns 1
   ```

### Agent uses wrong model

Check if the agent has a per-agent model override:

```bash
holon agent model get my-agent
```

Clear the override to fall back to the global default:

```bash
holon agent model clear my-agent
```

## Configuration Issues

### Config changes not taking effect

1. Verify the key was set correctly:
   ```bash
   holon config get <KEY>
   ```

2. Check for typos in the key name:
   ```bash
   holon config schema
   ```

3. If changes were made by editing `~/.holon/config.json` directly, validate the JSON:
   ```bash
   holon config list
   ```

4. Restart the daemon after config changes:
   ```bash
   holon daemon restart
   ```

### Config reset to defaults

Check if `HOLON_HOME` or `XDG_CONFIG_HOME` is set, which changes the config file location:

```bash
echo $HOLON_HOME
echo $XDG_CONFIG_HOME
```

## TUI Issues

### TUI display is garbled

Try disabling the alternate screen:

```bash
holon tui --no-alt-screen
```

Or set it in config:

```bash
holon config set tui.alternate_screen never
```

### TUI cannot connect to daemon

- Verify the daemon is running: `holon daemon status`
- Check the daemon's access mode: `holon daemon start --access local` is required for local TUI connections
- If connecting remotely, ensure the correct `--connect` URL and `--token`:
  ```bash
  holon tui --connect https://your-server:8787 --token "your-token"
  ```

## Logs & Diagnostics

### Viewing agent task output

Task output is stored under `~/.holon/agents/<agent-id>/task-output/`. Each task has a `.log` file named with its task ID.

### Full system diagnostic

```bash
holon config doctor
```

This reports: default model, fallback models, per-model availability with reasons, provider settings, credential status, and retry policy.

### Checking runtime configuration

```bash
# Full config dump
holon config list

# All available keys with defaults and descriptions
holon config schema
```

## See Also

- [Configuration Reference](/reference/configuration.md) — Configuration keys and credential management
- [Quick Examples](/guides/quick-examples.md) — Common task examples
- [Getting Started](/getting-started/first-agent.md) — Setup tutorial
